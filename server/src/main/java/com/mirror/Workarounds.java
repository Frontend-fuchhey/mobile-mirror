package com.mirror;

import android.app.Application;
import android.app.Instrumentation;
import android.content.Context;
import android.content.pm.ApplicationInfo;
import android.os.Build;

import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;

/**
 * Spoofs Android's internal ActivityThread state when running as a standalone
 * command-line DEX process under app_process (UID 2000 / shell).
 */
public final class Workarounds {
    private static final Class<?> ACTIVITY_THREAD_CLASS;
    private static final Object ACTIVITY_THREAD;

    static {
        try {
            ACTIVITY_THREAD_CLASS = Class.forName("android.app.ActivityThread");
            Constructor<?> constructor = ACTIVITY_THREAD_CLASS.getDeclaredConstructor();
            constructor.setAccessible(true);
            ACTIVITY_THREAD = constructor.newInstance();

            Field sCurrentActivityThreadField = ACTIVITY_THREAD_CLASS.getDeclaredField("sCurrentActivityThread");
            sCurrentActivityThreadField.setAccessible(true);
            sCurrentActivityThreadField.set(null, ACTIVITY_THREAD);

            Field mSystemThreadField = ACTIVITY_THREAD_CLASS.getDeclaredField("mSystemThread");
            mSystemThreadField.setAccessible(true);
            mSystemThreadField.setBoolean(ACTIVITY_THREAD, true);
        } catch (Exception e) {
            throw new AssertionError("Failed to initialize ActivityThread workaround", e);
        }
    }

    private Workarounds() {
    }

    public static void apply() {
        if (Build.VERSION.SDK_INT >= 31) {
            fillConfigurationController();
        }
        if (!"ONYX".equalsIgnoreCase(Build.BRAND)) {
            fillAppInfo();
        }
        fillAppContext();
    }

    private static void fillAppInfo() {
        try {
            Class<?> appBindDataClass = Class.forName("android.app.ActivityThread$AppBindData");
            Constructor<?> constructor = appBindDataClass.getDeclaredConstructor();
            constructor.setAccessible(true);
            Object appBindData = constructor.newInstance();

            ApplicationInfo applicationInfo = new ApplicationInfo();
            applicationInfo.packageName = FakeContext.PACKAGE_NAME;

            Field appInfoField = appBindDataClass.getDeclaredField("appInfo");
            appInfoField.setAccessible(true);
            appInfoField.set(appBindData, applicationInfo);

            Field mBoundApplicationField = ACTIVITY_THREAD_CLASS.getDeclaredField("mBoundApplication");
            mBoundApplicationField.setAccessible(true);
            mBoundApplicationField.set(ACTIVITY_THREAD, appBindData);
        } catch (Throwable t) {
            System.err.println("[MirrorServer] Warning: could not fill AppBindData: " + t.getMessage());
        }
    }

    private static void fillAppContext() {
        try {
            Application app = Instrumentation.newApplication(Application.class, FakeContext.get());
            Field mInitialApplicationField = ACTIVITY_THREAD_CLASS.getDeclaredField("mInitialApplication");
            mInitialApplicationField.setAccessible(true);
            mInitialApplicationField.set(ACTIVITY_THREAD, app);
        } catch (Throwable t) {
            System.err.println("[MirrorServer] Warning: could not fill mInitialApplication: " + t.getMessage());
        }
    }

    private static void fillConfigurationController() {
        try {
            Class<?> activityThreadInternalClass = Class.forName("android.app.ActivityThreadInternal");
            Class<?> configControllerClass = Class.forName("android.app.ConfigurationController");
            Constructor<?> constructor = configControllerClass.getDeclaredConstructor(activityThreadInternalClass);
            constructor.setAccessible(true);
            Object configController = constructor.newInstance(ACTIVITY_THREAD);

            Field mConfigurationControllerField = ACTIVITY_THREAD_CLASS.getDeclaredField("mConfigurationController");
            mConfigurationControllerField.setAccessible(true);
            mConfigurationControllerField.set(ACTIVITY_THREAD, configController);
        } catch (Throwable t) {
            System.err.println("[MirrorServer] Warning: could not fill ConfigurationController: " + t.getMessage());
        }
    }

    static Context getSystemContext() {
        try {
            Method getSystemContextMethod = ACTIVITY_THREAD_CLASS.getDeclaredMethod("getSystemContext");
            return (Context) getSystemContextMethod.invoke(ACTIVITY_THREAD);
        } catch (Throwable t) {
            System.err.println("[MirrorServer] Warning: could not get system context: " + t.getMessage());
            return null;
        }
    }
}
