package com.mirror;

import android.annotation.SuppressLint;
import android.os.Build;
import android.os.Looper;
import android.os.Process;

import com.mirror.audio.AudioCapture;
import com.mirror.net.NalStreamer;
import com.mirror.net.StreamServer;
import com.mirror.video.ScreenCapture;
import com.mirror.video.SurfaceEncoder;

import java.lang.reflect.Field;
import java.nio.channels.SocketChannel;

/**
 * Main entry point for the Android screen mirroring server.
 *
 * Execution command (UID 2000 / shell):
 *   adb push server/server.jar /data/local/tmp/server.jar
 *   adb forward tcp:12345 tcp:12345
 *   adb shell CLASSPATH=/data/local/tmp/server.jar app_process / com.mirror.Server
 */
public final class Server {
    private Server() {
    }

    public static void main(String... args) {
        Thread.setDefaultUncaughtExceptionHandler((thread, throwable) -> {
            System.err.println("[MirrorServer] Fatal exception on thread " + thread.getName() + ": " + throwable.getMessage());
            throwable.printStackTrace(System.err);
            System.exit(1);
        });

        try {
            prepareMainLooper();
            Workarounds.apply();

            Options options = Options.parse(args);
            System.out.println("[MirrorServer] Starting on Android API " + Build.VERSION.SDK_INT
                    + " (UID=" + Process.myUid() + ", PID=" + Process.myPid() + ")");

            runServer(options);
        } catch (Throwable t) {
            System.err.println("[MirrorServer] Server terminated with error: " + t.getMessage());
            t.printStackTrace(System.err);
            System.exit(1);
        }
    }

    private static void runServer(Options options) throws Exception {
        ScreenCapture capture = new ScreenCapture(options);
        SurfaceEncoder encoder = new SurfaceEncoder(capture, options);

        Runtime.getRuntime().addShutdownHook(new Thread(() -> {
            encoder.stop();
            capture.close();
        }, "mirror-cleanup"));

        try (StreamServer server = new StreamServer(options.getPort());
             SocketChannel clientChannel = server.acceptClient();
             NalStreamer streamer = new NalStreamer(clientChannel, options.shouldSendHeader());
             AudioCapture audioCapture = new AudioCapture()) {

            // Guarantee stream header is ALWAYS sent first before any audio or video frames
            capture.prepare();
            streamer.writeStreamHeader(capture.getVideoSize());

            audioCapture.start(streamer);

            // Listen for real-time control commands from the desktop client (e.g. [M] to toggle Mic ON/OFF)
            Thread controlThread = new Thread(() -> {
                java.nio.ByteBuffer cmdBuf = java.nio.ByteBuffer.allocate(1);
                try {
                    while (!Thread.currentThread().isInterrupted()) {
                        cmdBuf.clear();
                        int n = clientChannel.read(cmdBuf);
                        if (n < 0) {
                            break;
                        }
                        if (n > 0) {
                            byte cmd = cmdBuf.get(0);
                            if (cmd == 1 || cmd == 'M') {
                                audioCapture.setMicEnabled(true);
                            } else if (cmd == 0 || cmd == 'm') {
                                audioCapture.setMicEnabled(false);
                            }
                        }
                    }
                } catch (Throwable ignored) {
                }
            }, "client-control-reader");
            controlThread.setDaemon(true);
            controlThread.start();

            encoder.streamScreen(streamer);
        } finally {
            capture.close();
            System.out.println("[MirrorServer] Streaming session finished cleanly.");
        }
    }

    @SuppressWarnings("deprecation")
    @SuppressLint("DiscouragedPrivateApi")
    private static void prepareMainLooper() {
        Looper.prepare();
        synchronized (Looper.class) {
            try {
                Field sMainLooperField = Looper.class.getDeclaredField("sMainLooper");
                sMainLooperField.setAccessible(true);
                sMainLooperField.set(null, Looper.myLooper());
            } catch (ReflectiveOperationException e) {
                throw new AssertionError("Failed to bind sMainLooper", e);
            }
        }
    }
}
