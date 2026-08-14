package com.nxrates.sdk;

/** Raised when the native layer rejects a call. */
public class NxrException extends RuntimeException {

    /** Status code returned by the native call, or 0 when not applicable. */
    private final int status;

    NxrException(String message) {
        this(message, 0);
    }

    NxrException(String message, int status) {
        super(message);
        this.status = status;
    }

    NxrException(String message, Throwable cause) {
        super(message, cause);
        this.status = 0;
    }

    /** Native status code (`NXR_ERR_*`), or 0. */
    public int status() {
        return status;
    }
}
