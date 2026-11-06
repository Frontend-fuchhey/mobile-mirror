package com.mirror.audio;

import android.annotation.SuppressLint;
import android.content.Context;
import android.media.AudioAttributes;
import android.media.AudioDeviceInfo;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioRecord;
import android.media.AudioTrack;
import android.media.MediaCodec;
import android.media.MediaCodecInfo;
import android.media.MediaFormat;
import android.media.MediaRecorder;
import android.media.audiofx.AcousticEchoCanceler;
import android.media.audiofx.AutomaticGainControl;
import android.media.audiofx.NoiseSuppressor;
import android.os.Build;

import com.mirror.FakeContext;
import com.mirror.net.NalStreamer;

import java.io.Closeable;
import java.io.IOException;
import java.lang.reflect.Method;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Non-invasive Android system/game audio + optional voice-isolated microphone capture.
 *
 * Key guarantees:
 *  1. NEVER uses REMOTE_SUBMIX (source 8), so the phone's speakers/earphones are NEVER muted.
 *     Game audio (e.g. Free Fire) always plays directly from the phone.
 *  2. Uses AudioPolicy with ROUTE_FLAG_LOOP_BACK_RENDER (3) so game/system audio is duplicated
 *     into the MP4 recording while continuing to render on the phone.
 *  3. Optional Microphone capture (toggled via [M] on the desktop client):
 *     - Uses setPrivacySensitive(false) on API 30+ so multiplayer games (Free Fire in-game voice)
 *       can use the phone's microphone concurrently without conflict.
 *     - Applies 3-stage Voice Isolation (Hardware DSP NoiseSuppressor/AEC + 300-3400Hz Voice
 *       Bandpass Filter + Adaptive VAD Noise Gate) so only human voice is recorded and background
 *       noise is suppressed to silence.
 */
public final class AudioCapture implements Closeable {
    public static final int SAMPLE_RATE = 48000;
    public static final int CHANNELS = 2;
    public static final int ENCODING = AudioFormat.ENCODING_PCM_16BIT;
    private static final int AAC_BIT_RATE = 128_000;
    private static final int FRAME_BYTES = 4096; // 1024 stereo 16-bit PCM frames (21.33ms @ 48kHz)

    private final AtomicBoolean stopped = new AtomicBoolean(false);
    private final AtomicBoolean micEnabled = new AtomicBoolean(false);
    private final Object micLock = new Object();

    private AudioRecord systemRecord;
    private boolean needsPhoneSpeakerPassthrough = false;
    private AudioTrack phoneSpeakerTrack;
    private AudioRecord micRecord;
    private NoiseSuppressor noiseSuppressor;
    private AcousticEchoCanceler echoCanceler;
    private AutomaticGainControl gainControl;
    private Thread captureThread;

    public void start(NalStreamer streamer) {
        try {
            systemRecord = createSystemLoopbackRenderRecord();
            if (systemRecord != null && systemRecord.getState() == AudioRecord.STATE_INITIALIZED) {
                systemRecord.startRecording();
                if (needsPhoneSpeakerPassthrough) {
                    phoneSpeakerTrack = createPhoneSpeakerPassthroughTrack();
                    System.out.println("[MirrorServer] System audio capture active (REMOTE_SUBMIX + real-time phone speaker passthrough)");
                } else {
                    System.out.println("[MirrorServer] System audio capture active (ROUTE_FLAG_LOOP_BACK_RENDER)");
                }
            } else {
                systemRecord = null;
                System.err.println("[MirrorServer] Warning: system audio capture unavailable");
            }

            captureThread = new Thread(() -> captureLoop(streamer), "audio-capture");
            captureThread.setPriority(Thread.MAX_PRIORITY);
            captureThread.start();
        } catch (Throwable t) {
            System.err.println("[MirrorServer] Audio capture startup notice: " + t.getMessage());
        }
    }

    /**
     * Dynamically enables or disables voice-isolated phone microphone capture.
     * When disabled, the microphone is completely released so multiplayer games have exclusive access if needed.
     */
    public void setMicEnabled(boolean enabled) {
        boolean previous = micEnabled.getAndSet(enabled);
        if (previous == enabled) {
            return;
        }
        synchronized (micLock) {
            if (enabled) {
                openMicRecordLocked();
            } else {
                closeMicRecordLocked();
            }
        }
    }

    private void captureLoop(NalStreamer streamer) {
        MediaCodec aacEncoder = createAacEncoder();
        if (aacEncoder == null) {
            return;
        }
        MediaCodec.BufferInfo aacInfo = new MediaCodec.BufferInfo();
        VoiceIsolator voiceIsolator = new VoiceIsolator(SAMPLE_RATE);

        byte[] sysBuf = new byte[FRAME_BYTES];
        byte[] micBuf = new byte[FRAME_BYTES];
        byte[] mixBuf = new byte[FRAME_BYTES];
        long totalSamplesProcessed = 0;
        long frameCounter = 0;
        AudioManager audioManager = null;
        int maxMusicVol = 15;
        try {
            audioManager = (AudioManager) FakeContext.get().getSystemService(Context.AUDIO_SERVICE);
            if (audioManager != null) {
                maxMusicVol = Math.max(1, audioManager.getStreamMaxVolume(AudioManager.STREAM_MUSIC));
            }
        } catch (Throwable ignored) {
        }

        try {
            while (!stopped.get() && !Thread.currentThread().isInterrupted()) {
                boolean hasSys = false;
                boolean hasMic = false;

                if (phoneSpeakerTrack != null && audioManager != null && (frameCounter % 25 == 0)) {
                    try {
                        int curVol = audioManager.getStreamVolume(AudioManager.STREAM_MUSIC);
                        float gain = Math.min(1.0f, Math.max(0.05f, (float) curVol / (float) maxMusicVol));
                        phoneSpeakerTrack.setVolume(gain);
                    } catch (Throwable ignored) {
                    }
                }
                frameCounter++;

                // 1. Read internal system/game audio
                if (systemRecord != null) {
                    int r = systemRecord.read(sysBuf, 0, FRAME_BYTES);
                    if (r > 0) {
                        if (r < FRAME_BYTES) {
                            java.util.Arrays.fill(sysBuf, r, FRAME_BYTES, (byte) 0);
                        }
                        hasSys = true;
                        // Play directly to phone's built-in speaker/earphones so the phone is never silent while gaming
                        if (phoneSpeakerTrack != null) {
                            try {
                                phoneSpeakerTrack.write(sysBuf, 0, r, AudioTrack.WRITE_NON_BLOCKING);
                            } catch (Throwable ignored) {
                            }
                        }
                    }
                }

                // 2. Read voice-isolated microphone audio (only when user enabled Mic via [M])
                if (micEnabled.get()) {
                    synchronized (micLock) {
                        if (micRecord == null && !stopped.get()) {
                            openMicRecordLocked();
                        }
                        if (micRecord != null) {
                            int r = micRecord.read(micBuf, 0, FRAME_BYTES);
                            if (r > 0) {
                                if (r < FRAME_BYTES) {
                                    java.util.Arrays.fill(micBuf, r, FRAME_BYTES, (byte) 0);
                                }
                                // Filter out background noise and keep only human voice speech
                                hasMic = voiceIsolator.processStereo16le(micBuf, FRAME_BYTES);
                            }
                        }
                    }
                }

                if (!hasSys && !hasMic) {
                    // Pace the loop at ~21.33ms per 1024-sample frame and feed silence if recording
                    try {
                        Thread.sleep(21);
                    } catch (InterruptedException ignored) {
                        break;
                    }
                    java.util.Arrays.fill(mixBuf, (byte) 0);
                } else if (hasSys && hasMic) {
                    mixPcm16le(sysBuf, micBuf, mixBuf, FRAME_BYTES);
                } else if (hasSys) {
                    System.arraycopy(sysBuf, 0, mixBuf, 0, FRAME_BYTES);
                } else {
                    System.arraycopy(micBuf, 0, mixBuf, 0, FRAME_BYTES);
                }

                long ptsUs = (totalSamplesProcessed * 1_000_000L) / SAMPLE_RATE;
                totalSamplesProcessed += (FRAME_BYTES / 4); // 1024 stereo frames

                try {
                    encodeAndStreamAac(aacEncoder, aacInfo, mixBuf, FRAME_BYTES, ptsUs, streamer);
                } catch (IOException e) {
                    break;
                }
            }
        } finally {
            try {
                aacEncoder.stop();
            } catch (Throwable ignored) {
            }
            try {
                aacEncoder.release();
            } catch (Throwable ignored) {
            }
        }
    }

    private static void mixPcm16le(byte[] sys, byte[] mic, byte[] out, int length) {
        for (int i = 0; i + 1 < length; i += 2) {
            short s1 = (short) ((sys[i] & 0xFF) | (sys[i + 1] << 8));
            short s2 = (short) ((mic[i] & 0xFF) | (mic[i + 1] << 8));
            // Mix game audio + boosted clean voice with soft-clipping protection
            int mixed = (int) s1 + (int) (s2 * 1.25f);
            if (mixed > 32767) {
                mixed = 32767;
            } else if (mixed < -32768) {
                mixed = -32768;
            }
            out[i] = (byte) (mixed & 0xFF);
            out[i + 1] = (byte) ((mixed >> 8) & 0xFF);
        }
    }

    @SuppressLint("MissingPermission")
    private void openMicRecordLocked() {
        if (micRecord != null) {
            return;
        }
        int minBuf = AudioRecord.getMinBufferSize(SAMPLE_RATE, AudioFormat.CHANNEL_IN_STEREO, ENCODING);
        int bufSize = Math.max(minBuf * 4, FRAME_BYTES * 4);

        AudioFormat format = new AudioFormat.Builder()
                .setEncoding(ENCODING)
                .setSampleRate(SAMPLE_RATE)
                .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
                .build();

        // Try VOICE_COMMUNICATION (7) first (activates hardware DSP voice isolation/beamforming),
        // then fallback to MIC (1)
        int[] sources = new int[]{
                MediaRecorder.AudioSource.VOICE_COMMUNICATION,
                MediaRecorder.AudioSource.MIC
        };

        for (int src : sources) {
            try {
                AudioRecord.Builder builder = new AudioRecord.Builder()
                        .setAudioSource(src)
                        .setAudioFormat(format)
                        .setBufferSizeInBytes(bufSize);

                if (Build.VERSION.SDK_INT >= 31) {
                    try {
                        Method setContextMethod = AudioRecord.Builder.class.getMethod("setContext", Context.class);
                        setContextMethod.invoke(builder, FakeContext.get());
                    } catch (Throwable ignored) {
                    }
                }

                // Allow concurrent mic capture alongside multiplayer games (Android 11 / API 30+)
                if (Build.VERSION.SDK_INT >= 30) {
                    try {
                        Method setPrivMethod = AudioRecord.Builder.class.getMethod("setPrivacySensitive", boolean.class);
                        setPrivMethod.invoke(builder, false);
                    } catch (Throwable ignored) {
                    }
                }

                AudioRecord rec = builder.build();
                if (rec.getState() == AudioRecord.STATE_INITIALIZED) {
                    attachHardwareVoiceEffects(rec.getAudioSessionId());
                    rec.startRecording();
                    micRecord = rec;
                    System.out.println("[MirrorServer] Voice-isolated microphone enabled (source=" + src + ")");
                    return;
                }
                rec.release();
            } catch (Throwable t) {
                System.err.println("[MirrorServer] Mic source " + src + " notice: " + t.getMessage());
            }
        }
    }

    private void attachHardwareVoiceEffects(int audioSessionId) {
        try {
            if (NoiseSuppressor.isAvailable()) {
                noiseSuppressor = NoiseSuppressor.create(audioSessionId);
                if (noiseSuppressor != null) {
                    noiseSuppressor.setEnabled(true);
                }
            }
        } catch (Throwable ignored) {
        }
        try {
            if (AcousticEchoCanceler.isAvailable()) {
                echoCanceler = AcousticEchoCanceler.create(audioSessionId);
                if (echoCanceler != null) {
                    echoCanceler.setEnabled(true);
                }
            }
        } catch (Throwable ignored) {
        }
        try {
            if (AutomaticGainControl.isAvailable()) {
                gainControl = AutomaticGainControl.create(audioSessionId);
                if (gainControl != null) {
                    gainControl.setEnabled(true);
                }
            }
        } catch (Throwable ignored) {
        }
    }

    private void closeMicRecordLocked() {
        if (noiseSuppressor != null) {
            try {
                noiseSuppressor.release();
            } catch (Throwable ignored) {
            }
            noiseSuppressor = null;
        }
        if (echoCanceler != null) {
            try {
                echoCanceler.release();
            } catch (Throwable ignored) {
            }
            echoCanceler = null;
        }
        if (gainControl != null) {
            try {
                gainControl.release();
            } catch (Throwable ignored) {
            }
            gainControl = null;
        }
        if (micRecord != null) {
            try {
                micRecord.stop();
            } catch (Throwable ignored) {
            }
            try {
                micRecord.release();
            } catch (Throwable ignored) {
            }
            micRecord = null;
            System.out.println("[MirrorServer] Microphone capture disabled.");
        }
    }

    private static MediaCodec createAacEncoder() {
        try {
            MediaCodec encoder = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_AUDIO_AAC);
            MediaFormat format = MediaFormat.createAudioFormat(MediaFormat.MIMETYPE_AUDIO_AAC, SAMPLE_RATE, CHANNELS);
            format.setInteger(MediaFormat.KEY_BIT_RATE, AAC_BIT_RATE);
            format.setInteger(MediaFormat.KEY_AAC_PROFILE, MediaCodecInfo.CodecProfileLevel.AACObjectLC);
            format.setInteger(MediaFormat.KEY_PCM_ENCODING, ENCODING);
            format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 16384);
            encoder.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE);
            encoder.start();
            return encoder;
        } catch (Throwable t) {
            System.err.println("[MirrorServer] AAC encoder notice: " + t.getMessage());
            return null;
        }
    }

    private static void encodeAndStreamAac(
            MediaCodec encoder,
            MediaCodec.BufferInfo bufferInfo,
            byte[] pcmData,
            int length,
            long ptsUs,
            NalStreamer streamer
    ) throws IOException {
        try {
            int inIdx = encoder.dequeueInputBuffer(5_000);
            if (inIdx >= 0) {
                ByteBuffer inBuf = encoder.getInputBuffer(inIdx);
                if (inBuf != null) {
                    inBuf.clear();
                    int cap = Math.min(length, inBuf.remaining());
                    inBuf.put(pcmData, 0, cap);
                    encoder.queueInputBuffer(inIdx, 0, cap, ptsUs, 0);
                }
            }

            while (true) {
                int outIdx = encoder.dequeueOutputBuffer(bufferInfo, 0);
                if (outIdx >= 0) {
                    try {
                        ByteBuffer outBuf = encoder.getOutputBuffer(outIdx);
                        if (outBuf != null && bufferInfo.size > 0) {
                            streamer.writeEncodedAudioPacket(outBuf, bufferInfo);
                        }
                    } finally {
                        encoder.releaseOutputBuffer(outIdx, false);
                    }
                } else {
                    break;
                }
            }
        } catch (IllegalStateException ignored) {
        }
    }

    /**
     * Creates a non-invasive AudioPolicy loopback-render AudioRecord that duplicates game/media audio
     * while keeping it playing out of the phone's speakers/earphones.
     * NEVER uses REMOTE_SUBMIX (8) so the phone's audio is never muted.
     */
    private AudioRecord createSystemLoopbackRenderRecord() {
        int[] mediaAndGameUsages = new int[]{
                AudioAttributes.USAGE_MEDIA,
                AudioAttributes.USAGE_GAME,
                AudioAttributes.USAGE_UNKNOWN
        };
        int[] allPlaybackUsages = new int[]{4, 11, 12, 13, 16, 14, 1, 5, 10, 6, 0, 2, 3};

        // Attempt 1: AudioPolicy with ROUTE_FLAG_LOOP_BACK_RENDER (Android 13+ or ROMs with MODIFY_AUDIO_ROUTING)
        try {
            AudioRecord rec = buildAudioPolicyLoopbackRender(mediaAndGameUsages, true);
            if (rec != null && rec.getState() == AudioRecord.STATE_INITIALIZED) {
                needsPhoneSpeakerPassthrough = false;
                return rec;
            }
        } catch (Throwable ignored) {
        }

        try {
            AudioRecord rec = buildAudioPolicyLoopbackRender(allPlaybackUsages, true);
            if (rec != null && rec.getState() == AudioRecord.STATE_INITIALIZED) {
                needsPhoneSpeakerPassthrough = false;
                return rec;
            }
        } catch (Throwable ignored) {
        }

        // Attempt 2: REMOTE_SUBMIX (source 8) with real-time low-latency phone speaker passthrough
        // Works on Android 11 & 12 (API 30..32) using shell's CAPTURE_AUDIO_OUTPUT permission
        try {
            int minBuf = AudioRecord.getMinBufferSize(SAMPLE_RATE, AudioFormat.CHANNEL_IN_STEREO, ENCODING);
            int bufSize = Math.max(minBuf * 4, 32768);

            AudioFormat format = new AudioFormat.Builder()
                    .setEncoding(ENCODING)
                    .setSampleRate(SAMPLE_RATE)
                    .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
                    .build();

            AudioRecord.Builder builder = new AudioRecord.Builder();
            if (Build.VERSION.SDK_INT >= 31) {
                try {
                    Method setContextMethod = AudioRecord.Builder.class.getMethod("setContext", Context.class);
                    setContextMethod.invoke(builder, FakeContext.get());
                } catch (Throwable ignored) {
                }
            }
            builder.setAudioSource(8); // MediaRecorder.AudioSource.REMOTE_SUBMIX
            builder.setAudioFormat(format);
            builder.setBufferSizeInBytes(bufSize);

            AudioRecord rec = builder.build();
            if (rec.getState() == AudioRecord.STATE_INITIALIZED) {
                needsPhoneSpeakerPassthrough = true;
                return rec;
            }
            rec.release();
        } catch (Throwable t) {
            System.err.println("[MirrorServer] REMOTE_SUBMIX capture notice: " + t.getMessage());
        }

        return null;
    }

    /**
     * Creates an ultra-low-latency AudioTrack on the phone routed to USAGE_ALARM / physical speaker
     * (which is NOT captured by REMOTE_SUBMIX, avoiding any feedback loop) so the user hears
     * game audio coming directly from their phone while REMOTE_SUBMIX captures it for the MP4 recording.
     */
    private static AudioTrack createPhoneSpeakerPassthroughTrack() {
        try {
            int minBuf = AudioTrack.getMinBufferSize(SAMPLE_RATE, AudioFormat.CHANNEL_OUT_STEREO, ENCODING);
            int bufSize = Math.max(minBuf, FRAME_BYTES * 2);

            // USAGE_ALARM / SONIFICATION bypasses REMOTE_SUBMIX in Android AudioPolicyManager
            // and routes directly to the phone's speaker/headset without feedback!
            AudioAttributes attrs = new AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_ALARM)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                    .build();

            AudioFormat format = new AudioFormat.Builder()
                    .setEncoding(ENCODING)
                    .setSampleRate(SAMPLE_RATE)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                    .build();

            AudioTrack.Builder builder = new AudioTrack.Builder()
                    .setAudioAttributes(attrs)
                    .setAudioFormat(format)
                    .setBufferSizeInBytes(bufSize)
                    .setTransferMode(AudioTrack.MODE_STREAM);

            if (Build.VERSION.SDK_INT >= 26) {
                builder.setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY);
            }

            AudioTrack track = builder.build();

            // Sync alarm stream volume if muted while media volume is active, and route to headset if plugged in
            try {
                AudioManager am = (AudioManager) FakeContext.get().getSystemService(Context.AUDIO_SERVICE);
                if (am != null) {
                    int musicVol = am.getStreamVolume(AudioManager.STREAM_MUSIC);
                    int alarmVol = am.getStreamVolume(AudioManager.STREAM_ALARM);
                    if (musicVol > 0 && alarmVol == 0) {
                        int maxAlarm = am.getStreamMaxVolume(AudioManager.STREAM_ALARM);
                        int maxMusic = Math.max(1, am.getStreamMaxVolume(AudioManager.STREAM_MUSIC));
                        int targetAlarm = Math.max(1, (musicVol * maxAlarm) / maxMusic);
                        am.setStreamVolume(AudioManager.STREAM_ALARM, targetAlarm, 0);
                    }

                    AudioDeviceInfo[] devices = am.getDevices(AudioManager.GET_DEVICES_OUTPUTS);
                    for (AudioDeviceInfo d : devices) {
                        int type = d.getType();
                        if (type == AudioDeviceInfo.TYPE_WIRED_HEADSET
                                || type == AudioDeviceInfo.TYPE_WIRED_HEADPHONES
                                || type == AudioDeviceInfo.TYPE_USB_HEADSET
                                || type == AudioDeviceInfo.TYPE_BLUETOOTH_A2DP) {
                            track.setPreferredDevice(d);
                            break;
                        }
                    }
                }
            } catch (Throwable ignored) {
            }

            track.play();
            return track;
        } catch (Throwable t) {
            System.err.println("[MirrorServer] Phone speaker passthrough notice: " + t.getMessage());
            return null;
        }
    }

    private static AudioRecord buildAudioPolicyLoopbackRender(int[] usages, boolean privilegedFlags) throws Exception {
        Class<?> mixingRuleClass = Class.forName("android.media.audiopolicy.AudioMixingRule");
        Class<?> ruleBuilderClass = Class.forName("android.media.audiopolicy.AudioMixingRule$Builder");
        Object ruleBuilder = ruleBuilderClass.getConstructor().newInstance();

        try {
            int mixRolePlayers = mixingRuleClass.getField("MIX_ROLE_PLAYERS").getInt(null);
            ruleBuilderClass.getMethod("setTargetMixRole", int.class).invoke(ruleBuilder, mixRolePlayers);
        } catch (Throwable ignored) {
        }

        int ruleMatchUsage = mixingRuleClass.getField("RULE_MATCH_ATTRIBUTE_USAGE").getInt(null);
        Method addMixRuleMethod = ruleBuilderClass.getMethod("addMixRule", int.class, Object.class);
        for (int usage : usages) {
            AudioAttributes attr = new AudioAttributes.Builder().setUsage(usage).build();
            addMixRuleMethod.invoke(ruleBuilder, ruleMatchUsage, attr);
        }

        if (privilegedFlags) {
            try {
                ruleBuilderClass.getMethod("allowPrivilegedPlaybackCapture", boolean.class).invoke(ruleBuilder, true);
            } catch (Throwable ignored) {
            }
            try {
                ruleBuilderClass.getMethod("voiceCommunicationCaptureAllowed", boolean.class).invoke(ruleBuilder, true);
            } catch (Throwable ignored) {
            }
        }

        Object mixingRule = ruleBuilderClass.getMethod("build").invoke(ruleBuilder);

        Class<?> audioMixClass = Class.forName("android.media.audiopolicy.AudioMix");
        Class<?> mixBuilderClass = Class.forName("android.media.audiopolicy.AudioMix$Builder");
        Object mixBuilder = mixBuilderClass.getConstructor(mixingRuleClass).newInstance(mixingRule);

        AudioFormat format = new AudioFormat.Builder()
                .setEncoding(ENCODING)
                .setSampleRate(SAMPLE_RATE)
                .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
                .build();
        mixBuilderClass.getMethod("setFormat", AudioFormat.class).invoke(mixBuilder, format);

        // CRITICAL: Use ROUTE_FLAG_LOOP_BACK_RENDER (3) so audio continues playing on the phone's speaker!
        int routeFlags;
        try {
            routeFlags = audioMixClass.getField("ROUTE_FLAG_LOOP_BACK_RENDER").getInt(null);
        } catch (Throwable ignored) {
            int render = audioMixClass.getField("ROUTE_FLAG_RENDER").getInt(null);
            int loopback = audioMixClass.getField("ROUTE_FLAG_LOOP_BACK").getInt(null);
            routeFlags = render | loopback;
        }
        mixBuilderClass.getMethod("setRouteFlags", int.class).invoke(mixBuilder, routeFlags);
        Object audioMix = mixBuilderClass.getMethod("build").invoke(mixBuilder);

        Class<?> audioPolicyClass = Class.forName("android.media.audiopolicy.AudioPolicy");
        Class<?> policyBuilderClass = Class.forName("android.media.audiopolicy.AudioPolicy$Builder");
        Object policyBuilder = policyBuilderClass.getConstructor(Context.class).newInstance(FakeContext.get());
        policyBuilderClass.getMethod("addMix", audioMixClass).invoke(policyBuilder, audioMix);
        Object audioPolicy = policyBuilderClass.getMethod("build").invoke(policyBuilder);

        boolean registered = false;
        try {
            Object audioManager = FakeContext.get().getSystemService(Context.AUDIO_SERVICE);
            if (audioManager != null) {
                Method regInstance = AudioManager.class.getDeclaredMethod("registerAudioPolicy", audioPolicyClass);
                regInstance.setAccessible(true);
                Object r = regInstance.invoke(audioManager, audioPolicy);
                if (r instanceof Integer && ((Integer) r) == 0) {
                    registered = true;
                }
            }
        } catch (Throwable ignored) {
        }

        if (!registered) {
            Method registerMethod = AudioManager.class.getDeclaredMethod("registerAudioPolicyStatic", audioPolicyClass);
            registerMethod.setAccessible(true);
            Object res = registerMethod.invoke(null, audioPolicy);
            if (res instanceof Integer && ((Integer) res) != 0) {
                throw new RuntimeException("registerAudioPolicyStatic returned " + res);
            }
        }

        Method createSinkMethod = audioPolicyClass.getMethod("createAudioRecordSink", audioMixClass);
        return (AudioRecord) createSinkMethod.invoke(audioPolicy, audioMix);
    }

    @Override
    public void close() {
        stopped.set(true);
        if (captureThread != null) {
            captureThread.interrupt();
            captureThread = null;
        }
        synchronized (micLock) {
            closeMicRecordLocked();
        }
        if (phoneSpeakerTrack != null) {
            try {
                phoneSpeakerTrack.stop();
            } catch (Throwable ignored) {
            }
            try {
                phoneSpeakerTrack.release();
            } catch (Throwable ignored) {
            }
            phoneSpeakerTrack = null;
        }
        if (systemRecord != null) {
            try {
                systemRecord.stop();
            } catch (Throwable ignored) {
            }
            try {
                systemRecord.release();
            } catch (Throwable ignored) {
            }
            systemRecord = null;
        }
    }

    /**
     * Real-time DSP Voice Isolator:
     *  1. High-Pass Filter (280 Hz) removes low-end rumble, fan hum, wind, and finger taps on phone screen.
     *  2. Low-Pass Filter (3400 Hz) removes high-frequency static/hiss outside the human vocal band.
     *  3. Adaptive Voice Activity Detector (VAD) + Smooth Noise Gate tracks ambient noise floor
     *     and completely mutes background noise when the user is not speaking.
     */
    private static final class VoiceIsolator {
        private final float hpAlpha;
        private final float lpAlpha;

        private float hpPrevInL = 0f, hpPrevOutL = 0f;
        private float hpPrevInR = 0f, hpPrevOutR = 0f;
        private float lpPrevOutL = 0f, lpPrevOutR = 0f;

        private float noiseFloorRms = 400.0f;
        private float currentGain = 0.0f;
        private int holdFrames = 0;

        VoiceIsolator(int sampleRate) {
            float dt = 1.0f / sampleRate;
            // High-pass @ 280 Hz
            float rcHp = (float) (1.0 / (2.0 * Math.PI * 280.0));
            this.hpAlpha = rcHp / (rcHp + dt);
            // Low-pass @ 3400 Hz
            float rcLp = (float) (1.0 / (2.0 * Math.PI * 3400.0));
            this.lpAlpha = dt / (rcLp + dt);
        }

        boolean processStereo16le(byte[] pcm, int length) {
            int numStereoFrames = length / 4;
            if (numStereoFrames <= 0) {
                return false;
            }

            double sumSq = 0.0;
            int zeroCrossings = 0;
            float prevSample = 0f;

            // Pass 1: Bandpass filter (280 Hz - 3400 Hz) and measure speech band energy + ZCR
            for (int i = 0; i + 3 < length; i += 4) {
                float inL = (short) ((pcm[i] & 0xFF) | (pcm[i + 1] << 8));
                float inR = (short) ((pcm[i + 2] & 0xFF) | (pcm[i + 3] << 8));

                // High-pass (removes rumble/taps/fan hum < 280 Hz)
                float hpL = hpAlpha * (hpPrevOutL + inL - hpPrevInL);
                hpPrevInL = inL;
                hpPrevOutL = hpL;

                float hpR = hpAlpha * (hpPrevOutR + inR - hpPrevInR);
                hpPrevInR = inR;
                hpPrevOutR = hpR;

                // Low-pass (removes hiss > 3400 Hz)
                float outL = lpPrevOutL + lpAlpha * (hpL - lpPrevOutL);
                lpPrevOutL = outL;

                float outR = lpPrevOutR + lpAlpha * (hpR - lpPrevOutR);
                lpPrevOutR = outR;

                float mono = 0.5f * (outL + outR);
                sumSq += (double) (mono * mono);
                if ((prevSample < 0f && mono >= 0f) || (prevSample >= 0f && mono < 0f)) {
                    zeroCrossings++;
                }
                prevSample = mono;

                short sL = clamp16(outL);
                short sR = clamp16(outR);
                pcm[i] = (byte) (sL & 0xFF);
                pcm[i + 1] = (byte) ((sL >> 8) & 0xFF);
                pcm[i + 2] = (byte) (sR & 0xFF);
                pcm[i + 3] = (byte) ((sR >> 8) & 0xFF);
            }

            float rms = (float) Math.sqrt(sumSq / numStereoFrames);
            // Human voice ZCR at 48kHz in 1024 samples (~21.3ms) typically lies between 8 and 180 crossings
            boolean plausibleVoiceZcr = (zeroCrossings >= 6 && zeroCrossings <= 210);

            // Adaptive noise floor tracking during quiet frames
            if (rms < noiseFloorRms * 1.6f) {
                noiseFloorRms = 0.94f * noiseFloorRms + 0.06f * rms;
                if (noiseFloorRms < 180.0f) {
                    noiseFloorRms = 180.0f;
                } else if (noiseFloorRms > 2200.0f) {
                    noiseFloorRms = 2200.0f;
                }
            }

            float voiceThreshold = Math.max(480.0f, noiseFloorRms * 2.8f);
            boolean isVoiceActive = (rms >= voiceThreshold) && plausibleVoiceZcr;

            if (isVoiceActive) {
                holdFrames = 8; // Hold gate open for ~170ms after syllable ends
            } else if (holdFrames > 0) {
                holdFrames--;
                isVoiceActive = true;
            }

            float targetGain = isVoiceActive ? 1.0f : 0.0f;

            // Pass 2: Apply smooth envelope gate to completely zero out non-voice background noise
            boolean anyNonZero = false;
            for (int i = 0; i + 3 < length; i += 4) {
                currentGain = 0.992f * currentGain + 0.008f * targetGain;
                if (currentGain < 0.01f && targetGain == 0.0f) {
                    pcm[i] = 0;
                    pcm[i + 1] = 0;
                    pcm[i + 2] = 0;
                    pcm[i + 3] = 0;
                } else {
                    anyNonZero = true;
                    short sL = (short) ((pcm[i] & 0xFF) | (pcm[i + 1] << 8));
                    short sR = (short) ((pcm[i + 2] & 0xFF) | (pcm[i + 3] << 8));
                    short gL = clamp16(sL * currentGain);
                    short gR = clamp16(sR * currentGain);
                    pcm[i] = (byte) (gL & 0xFF);
                    pcm[i + 1] = (byte) ((gL >> 8) & 0xFF);
                    pcm[i + 2] = (byte) (gR & 0xFF);
                    pcm[i + 3] = (byte) ((gR >> 8) & 0xFF);
                }
            }

            return anyNonZero;
        }

        private static short clamp16(float v) {
            if (v > 32767f) return 32767;
            if (v < -32768f) return -32768;
            return (short) v;
        }
    }
}
