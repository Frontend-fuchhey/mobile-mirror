package com.mirror.video;

import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.os.Build;
import android.view.Surface;

import com.mirror.Options;
import com.mirror.display.Size;
import com.mirror.net.NalStreamer;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Hardware-accelerated H.264 Surface-to-Bitstream encoder using Android MediaCodec.
 * Configured for sub-50ms latency:
 *   - Color format: COLOR_FormatSurface (0x7F000789) for zero-copy GPU/SurfaceFlinger input
 *   - Bitrate: 12 Mbps Constant Bitrate (BITRATE_MODE_CBR)
 *   - Frame rate: 60 FPS
 *   - Zero B-frames (KEY_MAX_B_FRAMES = 0) & realtime priority (KEY_PRIORITY = 0)
 */
public final class SurfaceEncoder {
    private static final String MIME_TYPE_AVC = MediaFormat.MIMETYPE_VIDEO_AVC; // "video/avc"
    private static final String KEY_MAX_FPS_TO_ENCODER = "max-fps-to-encoder";
    private static final long REPEAT_FRAME_DELAY_US = 100_000L; // 100ms
    private static final long DEQUEUE_TIMEOUT_US = 10_000L;     // 10ms poll timeout

    private final ScreenCapture capture;
    private final Options options;
    private final AtomicBoolean stopped = new AtomicBoolean(false);

    public SurfaceEncoder(ScreenCapture capture, Options options) {
        this.capture = capture;
        this.options = options;
    }

    public void streamScreen(NalStreamer streamer) throws IOException {
        while (!stopped.get() && !Thread.currentThread().isInterrupted()) {
            capture.prepare();
            Size videoSize = capture.getVideoSize();

            MediaCodec codec = createHardwareEncoder(options.getEncoderName());
            Surface inputSurface = null;
            try {
                MediaFormat format = createLowLatencyFormat(videoSize, options, true);
                try {
                    codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
                } catch (Exception e) {
                    // Fallback if the device's hardware encoder rejects CBR or specific vendor keys
                    System.err.println("[MirrorServer] CBR/strict format rejected (" + e.getMessage()
                            + "), retrying with standard low-latency profile...");
                    try {
                        codec.reset();
                    } catch (Throwable ignored) {
                        try {
                            codec.release();
                        } catch (Throwable ignored2) {
                        }
                        codec = createHardwareEncoder(options.getEncoderName());
                    }
                    format = createLowLatencyFormat(videoSize, options, false);
                    codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
                }

                inputSurface = codec.createInputSurface();
                capture.start(inputSurface);
                codec.start();

                System.out.println("[MirrorServer] Hardware encoder started: " + codec.getName()
                        + " (" + videoSize + " @ " + options.getMaxFps() + " FPS, "
                        + (options.getBitRate() / 1_000_000) + " Mbps)");

                boolean displayChanged = encodeLoop(codec, streamer);
                if (!displayChanged) {
                    break;
                }
                System.out.println("[MirrorServer] Display orientation/dimensions changed, reconfiguring encoder...");
            } finally {
                capture.close();
                if (inputSurface != null) {
                    try {
                        inputSurface.release();
                    } catch (Throwable ignored) {
                    }
                }
                try {
                    codec.stop();
                } catch (Throwable ignored) {
                }
                try {
                    codec.release();
                } catch (Throwable ignored) {
                }
            }
        }
    }

    private boolean encodeLoop(MediaCodec codec, NalStreamer streamer) throws IOException {
        MediaCodec.BufferInfo bufferInfo = new MediaCodec.BufferInfo();
        long lastCheck = System.currentTimeMillis();

        while (!stopped.get() && !Thread.currentThread().isInterrupted()) {
            try {
                int outputIndex = codec.dequeueOutputBuffer(bufferInfo, DEQUEUE_TIMEOUT_US);
                if (outputIndex >= 0) {
                    try {
                        boolean eos = (bufferInfo.flags & MediaCodec.BUFFER_FLAG_END_OF_STREAM) != 0;
                        if (bufferInfo.size > 0) {
                            ByteBuffer outputBuffer = codec.getOutputBuffer(outputIndex);
                            if (outputBuffer != null) {
                                streamer.writePacket(outputBuffer, bufferInfo);
                            }
                        }
                        if (eos) {
                            return false;
                        }
                    } finally {
                        codec.releaseOutputBuffer(outputIndex, false);
                    }
                } else if (outputIndex == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                    MediaFormat newFormat = codec.getOutputFormat();
                    System.out.println("[MirrorServer] Encoder output format negotiated: " + newFormat);
                }
            } catch (IllegalStateException codecErr) {
                System.err.println("[MirrorServer] Encoder state transition (" + codecErr.getMessage() + "), reconfiguring...");
                return true;
            }

            long now = System.currentTimeMillis();
            if (now - lastCheck >= 200) {
                lastCheck = now;
                if (capture.checkDisplayChanged()) {
                    return true;
                }
            }
        }
        return false;
    }

    public void stop() {
        stopped.set(true);
    }

    private static MediaCodec createHardwareEncoder(String encoderName) throws IOException {
        if (encoderName != null && !encoderName.isEmpty()) {
            return MediaCodec.createByCodecName(encoderName);
        }
        return MediaCodec.createEncoderByType(MIME_TYPE_AVC);
    }

    private static MediaFormat createLowLatencyFormat(Size size, Options options, boolean includeAdvancedKeys) {
        MediaFormat format = MediaFormat.createVideoFormat(MIME_TYPE_AVC, size.getWidth(), size.getHeight());
        format.setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface);
        format.setInteger(MediaFormat.KEY_BIT_RATE, options.getBitRate());
        format.setInteger(MediaFormat.KEY_FRAME_RATE, options.getMaxFps());
        format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, options.getIFrameInterval());
        format.setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, REPEAT_FRAME_DELAY_US);

        if (options.getMaxFps() > 0) {
            format.setFloat(KEY_MAX_FPS_TO_ENCODER, (float) options.getMaxFps());
        }

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
            format.setInteger(MediaFormat.KEY_COLOR_RANGE, MediaFormat.COLOR_RANGE_LIMITED);
        }

        if (includeAdvancedKeys) {
            if (options.isCbr()) {
                format.setInteger(
                        MediaFormat.KEY_BITRATE_MODE,
                        MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR
                );
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
                // Realtime priority (0)
                format.setInteger(MediaFormat.KEY_PRIORITY, 0);
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                // Sub-frame / single-frame hardware latency
                format.setInteger(MediaFormat.KEY_LATENCY, 0);
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                // Eliminate B-frames to avoid frame reordering latency
                format.setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0);
            }
        }
        return format;
    }
}
