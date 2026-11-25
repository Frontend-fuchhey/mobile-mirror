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
import java.nio.ByteBuffer;
import java.nio.channels.SocketChannel;

/**
 * Main entry point for the Android USB screen mirroring server.
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
            System.err.println("[MirrorServer] Uncaught exception on thread " + thread.getName() + ": " + throwable.getMessage());
            throwable.printStackTrace(System.err);
            if ("main".equals(thread.getName())) {
                System.exit(1);
            }
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

        try (StreamServer server = new StreamServer(options.getPort())) {
            while (!Thread.currentThread().isInterrupted()) {
                try (SocketChannel clientChannel = server.acceptClient();
                     NalStreamer streamer = new NalStreamer(clientChannel, options.shouldSendHeader());
                     AudioCapture audioCapture = new AudioCapture()) {

                    encoder.resetForNewClient();

                    // Listen for real-time control commands from the desktop client (e.g. [M] to toggle Mic ON/OFF, [K] for instant IDR keyframe)
                    Thread controlThread = new Thread(() -> {
                        ByteBuffer cmdBuf = ByteBuffer.allocate(1);
                        try {
                            while (!Thread.currentThread().isInterrupted()) {
                                cmdBuf.clear();
                                int n = clientChannel.read(cmdBuf);
                                if (n < 0) {
                                    encoder.disconnectClient();
                                    break;
                                }
                                if (n > 0) {
                                    byte cmd = cmdBuf.get(0);
                                    if (cmd == 1 || cmd == 'M') {
                                        audioCapture.setMicEnabled(true);
                                    } else if (cmd == 0 || cmd == 'm') {
                                        audioCapture.setMicEnabled(false);
                                    } else if (cmd == 2 || cmd == 'K' || cmd == 'k') {
                                        encoder.requestSyncFrame();
                                    }
                                }
                            }
                        } catch (Throwable ignored) {
                            encoder.disconnectClient();
                        }
                    }, "client-control-reader");
                    controlThread.setDaemon(true);

                    encoder.streamScreen(streamer, () -> {
                        audioCapture.start(streamer);
                        controlThread.start();
                    });
                } catch (Throwable clientErr) {
                    System.err.println("[MirrorServer] Client session ended (" + clientErr.getMessage() + "), ready for immediate reconnect...");
                } finally {
                    capture.close();
                }
            }
        } finally {
            capture.close();
            System.out.println("[MirrorServer] Streaming session finished cleanly.");
        }
    }

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
