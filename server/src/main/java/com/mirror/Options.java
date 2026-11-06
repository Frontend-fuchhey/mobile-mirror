package com.mirror;

/**
 * Runtime configuration for the Android screen mirroring server.
 * Defaults to 12 Mbps CBR, 60 FPS, TCP port 12345, display 0.
 */
public final class Options {
    private int port = 12345;
    private int displayId = 0;
    private int maxSize = 0;
    private int maxFps = 60;
    private int bitRate = 12_000_000; // 12 Mbps default
    private int iFrameInterval = 2;   // 2 seconds between IDRs for fast recovery & MP4 seeking
    private boolean cbr = true;       // Constant Bitrate Mode (BITRATE_MODE_CBR)
    private boolean sendHeader = true; // Send 12-byte stream header + 12-byte per-packet metadata
    private String encoderName = null;

    public static Options parse(String... args) {
        Options options = new Options();
        if (args == null) {
            return options;
        }
        for (String arg : args) {
            if (arg == null || arg.isEmpty()) {
                continue;
            }
            String cleaned = arg.startsWith("--") ? arg.substring(2) : arg;
            int eq = cleaned.indexOf('=');
            if (eq == -1) {
                if ("raw".equalsIgnoreCase(cleaned)) {
                    options.sendHeader = false;
                } else if ("vbr".equalsIgnoreCase(cleaned)) {
                    options.cbr = false;
                }
                continue;
            }
            String key = cleaned.substring(0, eq).trim();
            String value = cleaned.substring(eq + 1).trim();
            switch (key) {
                case "port":
                    options.port = Integer.parseInt(value);
                    break;
                case "display_id":
                case "display":
                    options.displayId = Integer.parseInt(value);
                    break;
                case "max_size":
                    options.maxSize = Integer.parseInt(value);
                    break;
                case "max_fps":
                case "fps":
                    options.maxFps = Integer.parseInt(value);
                    break;
                case "bit_rate":
                case "bitrate":
                    options.bitRate = Integer.parseInt(value);
                    break;
                case "i_frame_interval":
                    options.iFrameInterval = Integer.parseInt(value);
                    break;
                case "cbr":
                    options.cbr = Boolean.parseBoolean(value);
                    break;
                case "send_header":
                case "framed":
                    options.sendHeader = Boolean.parseBoolean(value);
                    break;
                case "encoder":
                    options.encoderName = value;
                    break;
                default:
                    System.err.println("[MirrorServer] Ignoring unknown argument: " + arg);
                    break;
            }
        }
        return options;
    }

    public int getPort() {
        return port;
    }

    public int getDisplayId() {
        return displayId;
    }

    public int getMaxSize() {
        return maxSize;
    }

    public int getMaxFps() {
        return maxFps;
    }

    public int getBitRate() {
        return bitRate;
    }

    public int getIFrameInterval() {
        return iFrameInterval;
    }

    public boolean isCbr() {
        return cbr;
    }

    public boolean shouldSendHeader() {
        return sendHeader;
    }

    public String getEncoderName() {
        return encoderName;
    }
}
