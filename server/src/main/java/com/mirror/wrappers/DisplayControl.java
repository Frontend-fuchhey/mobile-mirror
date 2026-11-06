package com.mirror.wrappers;

import android.annotation.SuppressLint;
import android.os.IBinder;
import android.system.Os;

import java.lang.reflect.Method;

/**
 * Android 14+ (API 34+) moved getPhysicalDisplayIds and getPhysicalDisplayToken out of
 * SurfaceControl into com.android.server.display.DisplayControl inside services.jar.
 * This class dynamically loads DisplayControl from SYSTEMSERVERCLASSPATH and binds
 * libandroid_servers.so via Runtime.loadLibrary0.
 */
@SuppressLint({"BlockedPrivateApi", "PrivateApi", "SoonBlockedPrivateApi"})
public final class DisplayControl {
    private static final Class<?> CLASS;
    private static Method getPhysicalDisplayTokenMethod;
    private static Method getPhysicalDisplayIdsMethod;

    static {
        Class<?> loadedClass = null;
        try {
            Class<?> classLoaderFactoryClass = Class.forName("com.android.internal.os.ClassLoaderFactory");
            Method createClassLoaderMethod = classLoaderFactoryClass.getDeclaredMethod(
                    "createClassLoader",
                    String.class, String.class, String.class, ClassLoader.class,
                    int.class, boolean.class, String.class
            );
            String systemServerClasspath = Os.getenv("SYSTEMSERVERCLASSPATH");
            ClassLoader classLoader = (ClassLoader) createClassLoaderMethod.invoke(
                    null, systemServerClasspath, null, null,
                    ClassLoader.getSystemClassLoader(), 0, true, null
            );
            loadedClass = classLoader.loadClass("com.android.server.display.DisplayControl");

            Method loadLibrary0Method = Runtime.class.getDeclaredMethod("loadLibrary0", Class.class, String.class);
            loadLibrary0Method.setAccessible(true);
            loadLibrary0Method.invoke(Runtime.getRuntime(), loadedClass, "android_servers");
        } catch (Throwable ignored) {
            // Expected on Android < 14 where methods reside directly on SurfaceControl
        }
        CLASS = loadedClass;
    }

    private DisplayControl() {
    }

    private static Method getGetPhysicalDisplayTokenMethod() throws NoSuchMethodException {
        if (CLASS == null) {
            throw new NoSuchMethodException("DisplayControl unavailable");
        }
        if (getPhysicalDisplayTokenMethod == null) {
            getPhysicalDisplayTokenMethod = CLASS.getMethod("getPhysicalDisplayToken", long.class);
        }
        return getPhysicalDisplayTokenMethod;
    }

    public static IBinder getPhysicalDisplayToken(long physicalDisplayId) {
        try {
            return (IBinder) getGetPhysicalDisplayTokenMethod().invoke(null, physicalDisplayId);
        } catch (ReflectiveOperationException e) {
            return null;
        }
    }

    private static Method getGetPhysicalDisplayIdsMethod() throws NoSuchMethodException {
        if (CLASS == null) {
            throw new NoSuchMethodException("DisplayControl unavailable");
        }
        if (getPhysicalDisplayIdsMethod == null) {
            getPhysicalDisplayIdsMethod = CLASS.getMethod("getPhysicalDisplayIds");
        }
        return getPhysicalDisplayIdsMethod;
    }

    public static long[] getPhysicalDisplayIds() {
        try {
            return (long[]) getGetPhysicalDisplayIdsMethod().invoke(null);
        } catch (ReflectiveOperationException e) {
            return null;
        }
    }
}
