package com.mirror.net;

import android.media.MediaCodec;

import com.mirror.display.Size;

import java.io.Closeable;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.channels.SocketChannel;

/**
 * Zero-copy H.264 NAL unit streamer.
 *
 * Wire format when framed=true (default):
 *   1. Stream Header (12 bytes, Big-Endian):
 *      - u32 codec_id: 0x68323634 ("h264")
 *      - u32 width:    video width in pixels
 *      - u32 height:   video height in pixels
 *
 *   2. Per-Packet Frame Header (12 bytes, Big-Endian) + Raw Annex-B Payload:
 *      - u64 pts_and_flags:
 *          bit 63 (1L << 63): PACKET_FLAG_CONFIG (SPS/PPS codec config)
 *          bit 62 (1L << 62): PACKET_FLAG_KEY_FRAME (IDR keyframe)
 *          bits 0..61:        presentationTimeUs (microseconds)
 *      - u32 packet_size:     payload length in bytes
 *      - u8[packet_size]:     raw H.264 Annex-B NAL unit(s)
 *
 * When framed=false (--raw), only raw Annex-B NAL units are written directly.
 */
public final class NalStreamer implements Closeable {
    public static final int CODEC_ID_H264 = 0x68323634; // "h264"
    public static final long PACKET_FLAG_CONFIG = 1L << 63;
    public static final long PACKET_FLAG_KEY_FRAME = 1L << 62;
    public static final long PACKET_FLAG_AUDIO = 1L << 61;
    public static final long PTS_MASK = ~(PACKET_FLAG_CONFIG | PACKET_FLAG_KEY_FRAME | PACKET_FLAG_AUDIO);

    private final SocketChannel channel;
    private final boolean sendHeader;
    private final ByteBuffer headerBuffer;
    private final ByteBuffer[] gatherBuffers;

    public NalStreamer(SocketChannel channel, boolean sendHeader) {
        this.channel = channel;
        this.sendHeader = sendHeader;
        this.headerBuffer = ByteBuffer.allocateDirect(12).order(ByteOrder.BIG_ENDIAN);
        this.gatherBuffers = new ByteBuffer[]{this.headerBuffer, null};
    }

    /**
     * Sends the 12-byte stream header (codec magic + initial dimensions).
     */
    public synchronized void writeStreamHeader(Size videoSize) throws IOException {
        if (!sendHeader) {
            return;
        }
        headerBuffer.clear();
        headerBuffer.putInt(CODEC_ID_H264);
        headerBuffer.putInt(videoSize.getWidth());
        headerBuffer.putInt(videoSize.getHeight());
        headerBuffer.flip();
        writeFully(channel, headerBuffer);
    }

    /**
     * Streams a dequeued hardware encoder buffer directly to the socket channel
     * without copying NAL payload bytes onto the Java heap.
     */
    public synchronized void writePacket(ByteBuffer codecBuffer, MediaCodec.BufferInfo bufferInfo) throws IOException {
        codecBuffer.position(bufferInfo.offset);
        codecBuffer.limit(bufferInfo.offset + bufferInfo.size);

        if (!sendHeader) {
            writeFully(channel, codecBuffer);
            return;
        }

        boolean isConfig = (bufferInfo.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0;
        boolean isKeyFrame = (bufferInfo.flags & MediaCodec.BUFFER_FLAG_KEY_FRAME) != 0;

        long ptsAndFlags = bufferInfo.presentationTimeUs & PTS_MASK;
        if (isConfig) {
            ptsAndFlags |= PACKET_FLAG_CONFIG;
        }
        if (isKeyFrame) {
            ptsAndFlags |= PACKET_FLAG_KEY_FRAME;
        }

        headerBuffer.clear();
        headerBuffer.putLong(ptsAndFlags);
        headerBuffer.putInt(bufferInfo.size);
        headerBuffer.flip();

        writeFully(channel, headerBuffer);
        writeFully(channel, codecBuffer);
    }

    /**
     * Streams an uncompressed 48kHz Stereo 16-bit PCM audio frame to the socket channel
     * for ultra-low-latency live desktop speaker playback.
     */
    public synchronized void writeAudioPacket(byte[] data, int offset, int length, long ptsUs) throws IOException {
        if (!sendHeader) {
            return;
        }
        long ptsAndFlags = (ptsUs & PTS_MASK) | PACKET_FLAG_AUDIO;

        headerBuffer.clear();
        headerBuffer.putLong(ptsAndFlags);
        headerBuffer.putInt(length);
        headerBuffer.flip();

        ByteBuffer payload = ByteBuffer.wrap(data, offset, length);
        writeFully(channel, headerBuffer);
        writeFully(channel, payload);
    }

    /**
     * Streams a hardware-encoded AAC-LC (mp4a) audio packet or AudioSpecificConfig
     * to the socket channel for universal MP4 file recording.
     */
    public synchronized void writeEncodedAudioPacket(ByteBuffer codecBuffer, MediaCodec.BufferInfo bufferInfo) throws IOException {
        if (!sendHeader || bufferInfo.size <= 0) {
            return;
        }
        codecBuffer.position(bufferInfo.offset);
        codecBuffer.limit(bufferInfo.offset + bufferInfo.size);

        boolean isConfig = (bufferInfo.flags & MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0;
        long ptsAndFlags = (bufferInfo.presentationTimeUs & PTS_MASK) | PACKET_FLAG_AUDIO;
        if (isConfig) {
            ptsAndFlags |= PACKET_FLAG_CONFIG;
        } else {
            ptsAndFlags |= PACKET_FLAG_KEY_FRAME;
        }

        headerBuffer.clear();
        headerBuffer.putLong(ptsAndFlags);
        headerBuffer.putInt(bufferInfo.size);
        headerBuffer.flip();

        writeFully(channel, headerBuffer);
        writeFully(channel, codecBuffer);
    }

    private static void writeFully(SocketChannel ch, ByteBuffer buf) throws IOException {
        while (buf.hasRemaining()) {
            int written = ch.write(buf);
            if (written < 0) {
                throw new IOException("Socket channel closed");
            }
        }
    }

    @Override
    public void close() throws IOException {
        channel.close();
    }
}
