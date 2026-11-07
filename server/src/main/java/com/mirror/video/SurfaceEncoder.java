// [Mobile Mirror] Progressive implementation phase 1/2
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
    private static final long REPEAT_FRAME_DELAY_US = 16_667L; // 16.67ms (60 FPS continuous cadence)
    private static final long DEQUEUE_TIMEOUT_US = 10_000L;    // 10ms poll timeout

    private final ScreenCapture capture;
    private final Options options;
    private final AtomicBoolean stopped = new AtomicBoolean(false);
    private volatile MediaCodec activeCodec;

    public SurfaceEncoder(ScreenCapture capture, Options options) {
        this.capture = capture;
        this.options = options;
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
        while (!stopped.get() && !Thread.currentThread().isInterrupted()) {
            capture.prepare();
            Size videoSize = capture.getVideoSize();

            MediaCodec codec = createHardwareEncoder(options.getEncoderName());
            Surface inputSurface = null;
            try {
                // Tier 1: H.264 High Profile (CABAC + 8x8 DCT) + CBR + Low Latency
                boolean configured = false;
                try {
                    MediaFormat highFormat = createLowLatencyFormat(videoSize, options, true, true);
                    codec.configure(highFormat, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
                    configured = true;
                } catch (Exception e1) {
                    System.err.println("[MirrorServer] High Profile format rejected (" + e1.getMessage()
                            + "), retrying Tier 2 (Default Profile + CBR)...");
                    codec = resetOrRecreateCodec(codec, options.getEncoderName());
                }

                // Tier 2: Default Profile + CBR + Low Latency
                if (!configured) {
                    try {
                        MediaFormat cbrFormat = createLowLatencyFormat(videoSize, options, false, true);
                        codec.configure(cbrFormat, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
                        configured = true;
                    } catch (Exception e2) {
                        System.err.println("[MirrorServer] CBR/strict format rejected (" + e2.getMessage()
                                + "), retrying Tier 3 (Standard low-latency profile)...");
                        codec = resetOrRecreateCodec(codec, options.getEncoderName());
                    }
                }

                // Tier 3: Standard low-latency baseline fallback
                if (!configured) {
                    MediaFormat fallbackFormat = createLowLatencyFormat(videoSize, options, false, false);
                    codec.configure(fallbackFormat, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
                }

                inputSurface = codec.createInputSurface();
                capture.start(inputSurface);
                codec.start();
                this.activeCodec = codec;

                System.out.println("[MirrorServer] Hardware encoder started: " + codec.getName()
                        + " (" + videoSize + " @ " + options.getMaxFps() + " FPS, "
                        + (options.getBitRate() / 1_000_000) + " Mbps)");

                boolean displayChanged = encodeLoop(codec, streamer);
                if (!displayChanged) {
                    break;
                }
                System.out.println("[MirrorServer] Display orientation/dimensions changed, reconfiguring encoder...");
            } finally {
                this.activeCodec = null;
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
