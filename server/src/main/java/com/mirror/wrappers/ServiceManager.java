package com.mirror.wrappers;

import android.annotation.SuppressLint;
import android.os.IBinder;
import android.os.IInterface;

import java.lang.reflect.Method;

/**
 * Reflective binder service locator and DisplayManager singleton holder.
 */
@SuppressLint("PrivateApi")
public final class ServiceManager {
    private static final Method GET_SERVICE_METHOD;
    private static DisplayManager displayManager;

    static {
        try {
            GET_SERVICE_METHOD = Class.forName("android.os.ServiceManager")
                    .getDeclaredMethod("getService", String.class);
        } catch (Exception e) {
            throw new AssertionError("Failed to reflect android.os.ServiceManager", e);
        }
    }

    private ServiceManager() {
    }

    public static IInterface getService(String service, String type) {
        try {
            IBinder binder = (IBinder) GET_SERVICE_METHOD.invoke(null, service);
            Method asInterfaceMethod = Class.forName(type + "$Stub")
                    .getMethod("asInterface", IBinder.class);
            return (IInterface) asInterfaceMethod.invoke(null, binder);
        } catch (Exception e) {
            throw new AssertionError("Failed to resolve system service: " + service, e);
        }
    }

    public static synchronized DisplayManager getDisplayManager() {
        if (displayManager == null) {
            displayManager = DisplayManager.create();
        }
        return displayManager;
    }
}
