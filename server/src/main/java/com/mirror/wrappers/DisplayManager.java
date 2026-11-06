package com.mirror.wrappers;

import android.content.Context;
import android.hardware.display.VirtualDisplay;
import android.view.Display;
import android.view.Surface;

import com.mirror.FakeContext;
import com.mirror.display.DisplayInfo;
import com.mirror.display.Size;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.lang.reflect.Constructor;
import java.lang.reflect.Method;
import java.nio.charset.StandardCharsets;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Reflective wrapper around android.hardware.display.DisplayManagerGlobal and
 * hidden android.hardware.display.DisplayManager methods.
 */
public final class DisplayManager {
    private final Object manager;
    private Method getDisplayInfoMethod;
    private Method createVirtualDisplayMethod;

    public static DisplayManager create() {
        try {
            Class<?> clazz = Class.forName("android.hardware.display.DisplayManagerGlobal");
            Method getInstanceMethod = clazz.getDeclaredMethod("getInstance");
            Object dmGlobal = getInstanceMethod.invoke(null);
            return new DisplayManager(dmGlobal);
        } catch (ReflectiveOperationException e) {
            throw new AssertionError("Could not instantiate DisplayManagerGlobal", e);
        }
    }

    private DisplayManager(Object manager) {
        this.manager = manager;
    }

    private synchronized Method getGetDisplayInfoMethod() throws NoSuchMethodException {
        if (getDisplayInfoMethod == null) {
            getDisplayInfoMethod = manager.getClass().getMethod("getDisplayInfo", int.class);
        }
        return getDisplayInfoMethod;
    }

    public DisplayInfo getDisplayInfo(int displayId) {
        try {
            Method method = getGetDisplayInfoMethod();
            Object displayInfo = method.invoke(manager, displayId);
            if (displayInfo == null) {
                return getDisplayInfoFromDumpsysDisplay(displayId);
            }
            Class<?> cls = displayInfo.getClass();
            int width = cls.getDeclaredField("logicalWidth").getInt(displayInfo);
            int height = cls.getDeclaredField("logicalHeight").getInt(displayInfo);
            int rotation = cls.getDeclaredField("rotation").getInt(displayInfo);
            int layerStack = cls.getDeclaredField("layerStack").getInt(displayInfo);
            int flags = cls.getDeclaredField("flags").getInt(displayInfo);
            int dpi = cls.getDeclaredField("logicalDensityDpi").getInt(displayInfo);
            String uniqueId;
            try {
                uniqueId = (String) cls.getDeclaredField("uniqueId").get(displayInfo);
            } catch (NoSuchFieldException e) {
                uniqueId = null;
            }
            return new DisplayInfo(displayId, new Size(width, height), rotation, layerStack, flags, dpi, uniqueId);
        } catch (ReflectiveOperationException e) {
            throw new AssertionError("Failed to read DisplayInfo via reflection", e);
        }
    }

    public static DisplayInfo parseDisplayInfo(String dumpsysDisplayOutput, int displayId) {
        Pattern regex = Pattern.compile(
                "^    mOverrideDisplayInfo=DisplayInfo\\{\".*?, displayId " + displayId + ".*?"
                        + "(, FLAG_.*)?, real ([0-9]+) x ([0-9]+).*?, "
                        + "rotation ([0-9]+).*?, density ([0-9]+).*?, layerStack ([0-9]+)",
                Pattern.MULTILINE
        );
        Matcher matcher = regex.matcher(dumpsysDisplayOutput);
        if (!matcher.find()) {
            return null;
        }
        int flags = parseDisplayFlags(matcher.group(1));
        int width = Integer.parseInt(matcher.group(2));
        int height = Integer.parseInt(matcher.group(3));
        int rotation = Integer.parseInt(matcher.group(4));
        int density = Integer.parseInt(matcher.group(5));
        int layerStack = Integer.parseInt(matcher.group(6));

        return new DisplayInfo(displayId, new Size(width, height), rotation, layerStack, flags, density, null);
    }

    private static DisplayInfo getDisplayInfoFromDumpsysDisplay(int displayId) {
        try {
            Process process = new ProcessBuilder("dumpsys", "display").redirectErrorStream(true).start();
            StringBuilder sb = new StringBuilder();
            try (BufferedReader reader = new BufferedReader(
                    new InputStreamReader(process.getInputStream(), StandardCharsets.UTF_8))) {
                String line;
                while ((line = reader.readLine()) != null) {
                    sb.append(line).append('\n');
                }
            }
            process.waitFor();
            return parseDisplayInfo(sb.toString(), displayId);
        } catch (Exception e) {
            System.err.println("[MirrorServer] Could not parse dumpsys display: " + e.getMessage());
            return null;
        }
    }

    private static int parseDisplayFlags(String text) {
        if (text == null) {
            return 0;
        }
        int flags = 0;
        Pattern regex = Pattern.compile("FLAG_[A-Z_]+");
        Matcher matcher = regex.matcher(text);
        while (matcher.find()) {
            String flagString = matcher.group();
            try {
                flags |= Display.class.getDeclaredField(flagString).getInt(null);
            } catch (ReflectiveOperationException ignored) {
            }
        }
        return flags;
    }

    public int[] getDisplayIds() {
        try {
            return (int[]) manager.getClass().getMethod("getDisplayIds").invoke(manager);
        } catch (ReflectiveOperationException e) {
            throw new AssertionError("Failed to get display IDs", e);
        }
    }

    private Method getCreateVirtualDisplayMethod() throws NoSuchMethodException {
        if (createVirtualDisplayMethod == null) {
            createVirtualDisplayMethod = android.hardware.display.DisplayManager.class
                    .getMethod("createVirtualDisplay", String.class, int.class, int.class, int.class, Surface.class);
        }
        return createVirtualDisplayMethod;
    }

    /**
     * Invokes the hidden static method:
     * android.hardware.display.DisplayManager.createVirtualDisplay(String name, int width, int height, int displayIdToMirror, Surface surface)
     */
    public VirtualDisplay createVirtualDisplay(String name, int width, int height, int displayIdToMirror, Surface surface) throws Exception {
        Method method = getCreateVirtualDisplayMethod();
        return (VirtualDisplay) method.invoke(null, name, width, height, displayIdToMirror, surface);
    }

    /**
     * Creates a new virtual display using the hidden DisplayManager(Context) constructor and FakeContext.
     */
    public VirtualDisplay createNewVirtualDisplay(String name, int width, int height, int dpi, Surface surface, int flags) throws Exception {
        Constructor<android.hardware.display.DisplayManager> ctor =
                android.hardware.display.DisplayManager.class.getDeclaredConstructor(Context.class);
        ctor.setAccessible(true);
        android.hardware.display.DisplayManager dm = ctor.newInstance(FakeContext.get());
        return dm.createVirtualDisplay(name, width, height, dpi, surface, flags);
    }
}
