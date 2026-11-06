package com.mirror;

import android.annotation.TargetApi;
import android.content.AttributionSource;
import android.content.Context;
import android.content.ContextWrapper;
import android.os.Build;

import java.lang.reflect.Field;

/**
 * ContextWrapper that spoofs the ADB shell identity ("com.android.shell", UID 2000)
 * so privileged system services (DisplayManager, SurfaceControl, MediaCodec) grant
 * access without requiring interactive user consent dialogs.
 */
public final class FakeContext extends ContextWrapper {
    public static final String PACKAGE_NAME = "com.android.shell";
    public static final int SHELL_UID = 2000;
    public static final int ROOT_UID = 0;

    private static final FakeContext INSTANCE = new FakeContext();

    public static FakeContext get() {
        return INSTANCE;
    }

    private FakeContext() {
        super(Workarounds.getSystemContext());
    }

    @Override
    public String getPackageName() {
        return PACKAGE_NAME;
    }

    @Override
    public String getOpPackageName() {
        return PACKAGE_NAME;
    }

    @TargetApi(Build.VERSION_CODES.S)
    @Override
    public AttributionSource getAttributionSource() {
        AttributionSource.Builder builder = new AttributionSource.Builder(SHELL_UID);
        builder.setPackageName(PACKAGE_NAME);
        return builder.build();
    }

    @Override
    public int getDeviceId() {
        return 0;
    }

    @Override
    public Context getApplicationContext() {
        return this;
    }

    @Override
    public Context createPackageContext(String packageName, int flags) {
        return this;
    }

    @Override
    public Object getSystemService(String name) {
        Object service = super.getSystemService(name);
        if (service == null) {
            return null;
        }
        if ("clipboard".equals(name) || "activity".equals(name) || "audio".equals(name)) {
            try {
                Field mContextField = service.getClass().getDeclaredField("mContext");
                mContextField.setAccessible(true);
                mContextField.set(service, this);
            } catch (ReflectiveOperationException ignored) {
            }
        }
        return service;
    }
}
