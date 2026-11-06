package com.mirror.net;

import java.io.Closeable;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.StandardSocketOptions;
import java.nio.channels.ServerSocketChannel;
import java.nio.channels.SocketChannel;

/**
 * Low-latency local TCP server using NIO ServerSocketChannel and SocketChannel
 * to enable zero-copy direct ByteBuffer writes (writev) from MediaCodec.
 */
public final class StreamServer implements Closeable {
    private static final int SEND_BUFFER_SIZE = 2 * 1024 * 1024; // 2 MiB socket send buffer

    private final ServerSocketChannel serverChannel;
    private final int port;

    public StreamServer(int port) throws IOException {
        this.port = port;
        this.serverChannel = ServerSocketChannel.open();
        this.serverChannel.configureBlocking(true);
        this.serverChannel.setOption(StandardSocketOptions.SO_REUSEADDR, true);

        IOException lastException = null;
        for (int i = 0; i < 5; i++) {
            try {
                this.serverChannel.bind(new InetSocketAddress("127.0.0.1", port), 1);
                lastException = null;
                break;
            } catch (IOException e) {
                lastException = e;
                System.err.println("[MirrorServer] Port " + port + " busy, retrying in 250ms (attempt " + (i + 1) + "/5)...");
                try {
                    Thread.sleep(250);
                } catch (InterruptedException ignored) {
                    Thread.currentThread().interrupt();
                    break;
                }
            }
        }
        if (lastException != null) {
            throw lastException;
        }
    }

    /**
     * Waits for the desktop client to connect via ADB port forwarding (127.0.0.1:port)
     * and configures TCP_NODELAY for sub-50ms real-time streaming.
     */
    public SocketChannel acceptClient() throws IOException {
        System.out.println("[MirrorServer] Listening on 127.0.0.1:" + port + " (waiting for client)...");
        SocketChannel client = serverChannel.accept();
        client.configureBlocking(true);
        client.setOption(StandardSocketOptions.TCP_NODELAY, true);
        try {
            client.setOption(StandardSocketOptions.SO_SNDBUF, SEND_BUFFER_SIZE);
        } catch (IOException ignored) {
        }
        System.out.println("[MirrorServer] Client connected: " + client.getRemoteAddress());
        return client;
    }

    @Override
    public void close() throws IOException {
        serverChannel.close();
    }
}
