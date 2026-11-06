package com.mirror.video;

import android.graphics.Rect;
import android.hardware.display.VirtualDisplay;
import android.os.Build;
import android.os.IBinder;
import android.view.Surface;

import com.mirror.Options;
import com.mirror.display.DisplayInfo;
import com.mirror.display.Size;
import com.mirror.wrappers.ServiceManager;
import com.mirror.wrappers.SurfaceControl;

/**
 * Captures the physical screen without user consent dialogs by reflecting into
 * android.view.SurfaceControl and android.hardware.display.DisplayManager,
 * binding the compositor output directly to the MediaCodec input Surface.
 */
public final class ScreenCapture implements AutoCloseable {
    private static final int MACROBLOCK_ALIGNMENT = 16;

    private final int displayId;
    private final int maxSize;
    private DisplayInfo displayInfo;
    private Size videoSize;
    private IBinder surfaceControlDisplay;
    private VirtualDisplay virtualDisplay;

    public ScreenCapture(Options options) {
        this.displayId = options.getDisplayId();
        this.maxSize = options.getMaxSize();
    }

    /**
     * Queries physical display dimensions, rotation, and layerStack from DisplayManagerGlobal.
     */
    public void prepare() {
        this.displayInfo = ServiceManager.getDisplayManager().getDisplayInfo(displayId);
        if (this.displayInfo == null) {
            throw new IllegalStateException("Physical display " + displayId + " not found");
        }
        if ((displayInfo.getFlags() & DisplayInfo.FLAG_SUPPORTS_PROTECTED_BUFFERS) == 0) {
            System.err.println("[MirrorServer] Warning: display does not have FLAG_SUPPORTS_PROTECTED_BUFFERS");
        }
        this.videoSize = displayInfo.getSize().constrainAndAlign(maxSize, MACROBLOCK_ALIGNMENT);
        System.out.println("[MirrorServer] Display " + displayId + ": physical=" + displayInfo.getSize()
                + ", encoded=" + videoSize + ", rotation=" + displayInfo.getRotation()
                + ", layerStack=" + displayInfo.getLayerStack());
    }

    /**
     * Attaches the hardware MediaCodec input Surface to the physical display compositor.
     * Attempts SurfaceControl transaction binding first and falls back to DisplayManager.createVirtualDisplay
     * (compatible with Android 5 through Android 16 under shell UID 2000).
     */
    public void start(Surface encoderInputSurface) {
        releaseDisplayResources();

        Rect layerStackRect = displayInfo.getSize().toRect();
        Rect displayRect = videoSize.toRect();

        try {
            // Strategy 1: Reflective SurfaceControl transaction binding
            boolean secure = Build.VERSION.SDK_INT < Build.VERSION_CODES.R
                    || (Build.VERSION.SDK_INT == Build.VERSION_CODES.R && !"S".equals(Build.VERSION.CODENAME));
            this.surfaceControlDisplay = SurfaceControl.createDisplay("mirror", secure);
            SurfaceControl.openTransaction();
            try {
                SurfaceControl.setDisplaySurface(surfaceControlDisplay, encoderInputSurface);
                SurfaceControl.setDisplayProjection(surfaceControlDisplay, 0, layerStackRect, displayRect);
                SurfaceControl.setDisplayLayerStack(surfaceControlDisplay, displayInfo.getLayerStack());
            } finally {
                SurfaceControl.closeTransaction();
            }
            System.out.println("[MirrorServer] Bound encoder surface via SurfaceControl API");
        } catch (Exception surfaceControlError) {
            // Strategy 2: Reflective DisplayManager.createVirtualDisplay (Android 12/13/14/15/16+)
            try {
                this.virtualDisplay = ServiceManager.getDisplayManager().createVirtualDisplay(
                        "mirror",
                        videoSize.getWidth(),
                        videoSize.getHeight(),
                        displayId,
                        encoderInputSurface
                );
                System.out.println("[MirrorServer] Bound encoder surface via DisplayManager.createVirtualDisplay API");
            } catch (Exception dmError) {
                throw new IllegalStateException(
                        "Could not capture display via SurfaceControl (" + surfaceControlError.getMessage()
                                + ") or DisplayManager (" + dmError.getMessage() + ")",
                        dmError
                );
            }
        }
    }

    public Size getVideoSize() {
        return videoSize;
    }

    public DisplayInfo getDisplayInfo() {
        return displayInfo;
    }

    /**
     * Checks if the physical display orientation or dimensions have changed
     * (e.g. launching a landscape game or rotating the device).
     */
    public boolean checkDisplayChanged() {
        try {
            DisplayInfo current = ServiceManager.getDisplayManager().getDisplayInfo(displayId);
            if (current == null || displayInfo == null) {
                return false;
            }
            return current.getRotation() != displayInfo.getRotation()
                    || !current.getSize().equals(displayInfo.getSize());
        } catch (Throwable ignored) {
            return false;
        }
    }

    private void releaseDisplayResources() {
        if (surfaceControlDisplay != null) {
            try {
                SurfaceControl.destroyDisplay(surfaceControlDisplay);
            } catch (Throwable ignored) {
            }
            surfaceControlDisplay = null;
        }
        if (virtualDisplay != null) {
            try {
                virtualDisplay.release();
            } catch (Throwable ignored) {
            }
            virtualDisplay = null;
        }
    }

    @Override
    public void close() {
        releaseDisplayResources();
    }
}
