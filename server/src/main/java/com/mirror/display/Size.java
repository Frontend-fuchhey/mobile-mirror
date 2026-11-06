package com.mirror.display;

import android.graphics.Rect;

import java.util.Objects;

/**
 * Immutable 2D resolution with hardware encoder macroblock alignment and max-size scaling.
 */
public final class Size {
    private final int width;
    private final int height;

    public Size(int width, int height) {
        this.width = width;
        this.height = height;
    }

    public int getWidth() {
        return width;
    }

    public int getHeight() {
        return height;
    }

    /**
     * Constrains the maximum dimension to maxSize (if > 0) and aligns both dimensions
     * to a multiple of alignment (e.g. 8 or 16 pixels for H.264 hardware encoders).
     */
    public Size constrainAndAlign(int maxSize, int alignment) {
        int w = width;
        int h = height;

        if (maxSize > 0) {
            int major = Math.max(w, h);
            if (major > maxSize) {
                if (w >= h) {
                    h = (int) Math.round((double) h * maxSize / w);
                    w = maxSize;
                } else {
                    w = (int) Math.round((double) w * maxSize / h);
                    h = maxSize;
                }
            }
        }

        int mask = ~(alignment - 1);
        w = Math.max(alignment, w & mask);
        h = Math.max(alignment, h & mask);
        return new Size(w, h);
    }

    public Rect toRect() {
        return new Rect(0, 0, width, height);
    }

    @Override
    public boolean equals(Object o) {
        if (this == o) return true;
        if (!(o instanceof Size)) return false;
        Size size = (Size) o;
        return width == size.width && height == size.height;
    }

    @Override
    public int hashCode() {
        return Objects.hash(width, height);
    }

    @Override
    public String toString() {
        return width + "x" + height;
    }
}
