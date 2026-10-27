export type Platform = "darwin" | "win32" | "linux";

/**
 * Detects the current platform through the native bridge when available,
 * falling back to user agent detection.
 */
export function getPlatform(): Platform {
  // Try the native bridge first
  if (typeof window !== "undefined" && window.retoneAPI?.getPlatform) {
    const platform = window.retoneAPI.getPlatform();
    if (platform === "darwin" || platform === "win32" || platform === "linux") {
      return platform;
    }
  }

  // Fallback to user agent detection
  if (typeof navigator !== "undefined") {
    const ua = navigator.userAgent.toLowerCase();
    if (ua.includes("mac")) return "darwin";
    if (ua.includes("win")) return "win32";
    if (ua.includes("linux")) return "linux";
  }

  // Default to darwin
  return "darwin";
}

/**
 * Cached platform value for performance
 */
let cachedPlatform: Platform | null = null;

export function getCachedPlatform(): Platform {
  if (cachedPlatform === null) {
    cachedPlatform = getPlatform();
  }
  return cachedPlatform;
}
