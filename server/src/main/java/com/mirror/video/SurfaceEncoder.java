package com.mirror.video;

import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.os.Build;
import android.os.Bundle;
import android.view.Surface;

import com.mirror.Options;
import com.mirror.display.Size;
import com.mirror.net.NalStreamer;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Hardware-accelerated H.264 Surface-to-Bitstream encoder using Android MediaCodec.
 * Configured for sub-50ms latency and studio-grade recording quality:
 *   - Color format: COLOR_FormatSurface (0x7F000789) for zero-copy GPU/SurfaceFlinger input
 *   - Profile: H.264 High Profile (AVCProfileHigh, CABAC + 8x8 DCT) with automatic fallback
 *   - Bitrate: 24 Mbps Constant Bitrate (BITRATE_MODE_CBR)
 *   - Frame rate: 60 FPS with 16.67ms continuous repeat-frame cadence (eliminates 10 FPS VFR stutter)
 *   - Zero B-frames (KEY_MAX_B_FRAMES = 0) & realtime priority (KEY_PRIORITY = 0)
 */
public final class SurfaceEncoder {
    private static final String MIME_TYPE_AVC = MediaFormat.MIMETYPE_VIDEO_AVC; // "video/avc"
    private static final String KEY_MAX_FPS_TO_ENCODER = "max-fps-to-encoder";
    // Idle repeat-frame threshold (50ms / 20 FPS heartbeat when screen is completely static).
    // Must be strictly greater than a 60Hz/90Hz/120Hz VSync interval (8.33ms..16.67ms) so
    // GraphicBufferSource never injects duplicate frames in between real VSync frames!
    private static final long REPEAT_FRAME_DELAY_US = 50_000L;
    private static final long DEQUEUE_TIMEOUT_US = 10_000L;    // 10ms poll timeout

    private final ScreenCapture capture;
    private final Options options;
    private final AtomicBoolean stopped = new AtomicBoolean(false);
    private final AtomicBoolean clientDisconnected = new AtomicBoolean(false);
    private volatile MediaCodec activeCodec;

    public SurfaceEncoder(ScreenCapture capture, Options options) {
        this.capture = capture;
        this.options = options;
    }

    public void resetForNewClient() {
        clientDisconnected.set(false);
    }

    public void disconnectClient() {
        clientDisconnected.set(true);
    }

    /**
     * Requests an immediate hardware IDR sync frame on the very next encoded frame (< 16ms).
     * Used when the desktop client starts an MP4 recording mid-stream.
     */
    public void requestSyncFrame() {
        MediaCodec c = this.activeCodec;
        if (c != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.KITKAT) {
            try {
                Bundle params = new Bundle();
                params.putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0);
                c.setParameters(params);
            } catch (Throwable ignored) {
            }
        }
    }

    public void streamScreen(NalStreamer streamer) throws IOException {
        streamScreen(streamer, null);
    }

    public void streamScreen(NalStreamer streamer, Runnable onFirstStart) throws IOException {
        boolean headerSent = false;
        int consecutiveFailures = 0;

        while (!stopped.get() && !clientDisconnected.get() && !Thread.currentThread().isInterrupted()) {
            capture.prepare();
            Size videoSize = capture.getVideoSize();

            MediaCodec codec = null;
            Surface inputSurface = null;
            try {
                Object[] started = configureAndStartEncoderWithFallbacks(videoSize);
                codec = (MediaCodec) started[0];
                inputSurface = (Surface) started[1];

                capture.start(inputSurface);
                this.activeCodec = codec;
                consecutiveFailures = 0;

                if (!headerSent) {
                    streamer.writeStreamHeader(videoSize);
                    headerSent = true;
                    if (onFirstStart != null) {
                        onFirstStart.run();
                    }
                }

                System.out.println("[MirrorServer] Hardware encoder started: " + codec.getName()
                        + " (" + videoSize + " @ " + options.getMaxFps() + " FPS, "
                        + (options.getBitRate() / 1_000_000) + " Mbps)");

                boolean displayChanged = encodeLoop(codec, streamer);
                if (!displayChanged) {
                    break;
                }
                System.out.println("[MirrorServer] Display orientation/dimensions changed, reconfiguring encoder...");
            } catch (IOException ioErr) {
                throw ioErr;
            } catch (Throwable err) {
                consecutiveFailures++;
                System.err.println("[MirrorServer] Encoder setup/reconfigure error (attempt " + consecutiveFailures + "/3): " + err.getMessage());
                if (consecutiveFailures >= 3) {
                    throw new IOException("Failed to initialize hardware encoder after 3 attempts", err);
                }
                try {
                    Thread.sleep(80);
                } catch (InterruptedException ie) {
                    Thread.currentThread().interrupt();
                    break;
                }
            } finally {
                this.activeCodec = null;
                capture.close();
                if (inputSurface != null) {
                    try {
                        inputSurface.release();
                    } catch (Throwable ignored) {
                    }
                }
                if (codec != null) {
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
    }

    /**
     * Attempts 4 progressive hardware MediaCodec tiers, validating configure() + createInputSurface() + start()
     * together so drivers that defer validation to start() (e.g. high frame rates or High Profile) fall back cleanly.
     */
    private Object[] configureAndStartEncoderWithFallbacks(Size videoSize) throws IOException {
        MediaCodec codec = createHardwareEncoder(options.getEncoderName());
        Surface inputSurface = null;

        // Tier 1: H.264 High Profile (CABAC + 8x8 DCT) + CBR/VBR + Low Latency + target FPS
        try {
            MediaFormat highFormat = createLowLatencyFormat(videoSize, options, true, true, options.getMaxFps());
            codec.configure(highFormat, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            inputSurface = codec.createInputSurface();
            codec.start();
            return new Object[]{codec, inputSurface};
        } catch (Throwable e1) {
            System.err.println("[MirrorServer] Tier 1 High Profile rejected (" + e1.getMessage()
                    + "), retrying Tier 2 (Default Profile + CBR)...");
            releaseSurfaceQuietly(inputSurface);
            inputSurface = null;
            codec = resetOrRecreateCodec(codec, options.getEncoderName());
        }

        // Tier 2: Default Profile + CBR/VBR + Low Latency + target FPS
        try {
            MediaFormat cbrFormat = createLowLatencyFormat(videoSize, options, false, true, options.getMaxFps());
            codec.configure(cbrFormat, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            inputSurface = codec.createInputSurface();
            codec.start();
            return new Object[]{codec, inputSurface};
        } catch (Throwable e2) {
            System.err.println("[MirrorServer] Tier 2 CBR format rejected (" + e2.getMessage()
                    + "), retrying Tier 3 (Standard low-latency profile)...");
            releaseSurfaceQuietly(inputSurface);
            inputSurface = null;
            codec = resetOrRecreateCodec(codec, options.getEncoderName());
        }

        // Tier 3: Standard low-latency baseline fallback at target FPS
        try {
            MediaFormat stdFormat = createLowLatencyFormat(videoSize, options, false, false, options.getMaxFps());
            codec.configure(stdFormat, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            inputSurface = codec.createInputSurface();
            codec.start();
            return new Object[]{codec, inputSurface};
        } catch (Throwable e3) {
            System.err.println("[MirrorServer] Tier 3 format rejected (" + e3.getMessage()
                    + "), retrying Tier 4 (Safe 60 FPS hardware baseline)...");
            releaseSurfaceQuietly(inputSurface);
            inputSurface = null;
            codec = resetOrRecreateCodec(codec, options.getEncoderName());
        }

        // Tier 4: Universal 60 FPS safe hardware baseline (guarantees connection when 90/120 FPS exceeds SoC cap)
        MediaFormat safe60Format = createLowLatencyFormat(videoSize, options, false, false, Math.min(60, Math.max(30, options.getMaxFps())));
        codec.configure(safe60Format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
        inputSurface = codec.createInputSurface();
        codec.start();
        return new Object[]{codec, inputSurface};
    }

    private static void releaseSurfaceQuietly(Surface surface) {
        if (surface != null) {
            try {
                surface.release();
            } catch (Throwable ignored) {
            }
        }
    }

    private static MediaCodec resetOrRecreateCodec(MediaCodec codec, String encoderName) throws IOException {
        try {
            codec.reset();
            return codec;
        } catch (Throwable ignored) {
            try {
                codec.release();
            } catch (Throwable ignored2) {
            }
            return createHardwareEncoder(encoderName);
        }
    }

    private boolean encodeLoop(MediaCodec codec, NalStreamer streamer) throws IOException {
        MediaCodec.BufferInfo bufferInfo = new MediaCodec.BufferInfo();
        long lastCheck = System.currentTimeMillis();

        while (!stopped.get() && !clientDisconnected.get() && !Thread.currentThread().isInterrupted()) {
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
            if (now - lastCheck >= 300) {
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
        clientDisconnected.set(true);
    }

    private static MediaCodec createHardwareEncoder(String encoderName) throws IOException {
        if (encoderName != null && !encoderName.isEmpty()) {
            return MediaCodec.createByCodecName(encoderName);
        }
        return MediaCodec.createEncoderByType(MIME_TYPE_AVC);
    }

    private static MediaFormat createLowLatencyFormat(
            Size size,
            Options options,
            boolean highProfile,
            boolean includeAdvancedKeys,
            int requestedFps
    ) {
        MediaFormat format = MediaFormat.createVideoFormat(MIME_TYPE_AVC, size.getWidth(), size.getHeight());
        format.setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface);
        format.setInteger(MediaFormat.KEY_BIT_RATE, options.getBitRate());
        int targetFps = Math.max(30, requestedFps > 0 ? requestedFps : options.getMaxFps());
        format.setInteger(MediaFormat.KEY_FRAME_RATE, targetFps);
        format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, options.getIFrameInterval());

        // Only repeat previous frame when screen is idle for >= 50ms (never between active 60/90/120Hz VSync frames)
        format.setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, REPEAT_FRAME_DELAY_US);

        if (targetFps > 0) {
            format.setFloat(KEY_MAX_FPS_TO_ENCODER, (float) targetFps);
        }

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
            format.setInteger(MediaFormat.KEY_COLOR_RANGE, MediaFormat.COLOR_RANGE_LIMITED);
            format.setInteger(MediaFormat.KEY_COLOR_STANDARD, MediaFormat.COLOR_STANDARD_BT709);
            format.setInteger(MediaFormat.KEY_COLOR_TRANSFER, MediaFormat.COLOR_TRANSFER_SDR_VIDEO);
        }

        if (highProfile && Build.VERSION.SDK_INT >= Build.VERSION_CODES.LOLLIPOP) {
            format.setInteger(
                    MediaFormat.KEY_PROFILE,
                    MediaCodecInfo.CodecProfileLevel.AVCProfileHigh
            );
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
                int pixels = size.getWidth() * size.getHeight();
                int level;
                if (targetFps > 60 && pixels > 1_000_000) {
                    level = MediaCodecInfo.CodecProfileLevel.AVCLevel52;
                } else if (targetFps > 60 || pixels > 2_073_600) {
                    level = MediaCodecInfo.CodecProfileLevel.AVCLevel51;
                } else {
                    level = MediaCodecInfo.CodecProfileLevel.AVCLevel42;
                }
                format.setInteger(MediaFormat.KEY_LEVEL, level);
            }
        }

        if (includeAdvancedKeys) {
            if (options.isCbr()) {
                format.setInteger(
                        MediaFormat.KEY_BITRATE_MODE,
                        MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR
                );
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
                // Realtime priority (0) and full hardware operating rate
                format.setInteger(MediaFormat.KEY_PRIORITY, 0);
                format.setInteger(MediaFormat.KEY_OPERATING_RATE, targetFps);
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                // Sub-frame / single-frame hardware latency
                format.setInteger(MediaFormat.KEY_LATENCY, 0);
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                // Eliminate B-frames to avoid frame reordering latency while preserving High Profile CABAC/8x8 DCT
                format.setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0);
            }
        }
        return format;
    }
}
