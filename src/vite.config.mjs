import { defineConfig, loadEnv } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import path from "path";
import { fileURLToPath } from "url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const DEFAULT_DEV_SERVER_PORT = 5183;

const parseDevServerPort = (rawPort) => {
  const normalizedPort = rawPort || String(DEFAULT_DEV_SERVER_PORT);
  const parsedPort = Number(normalizedPort);

  if (!Number.isInteger(parsedPort) || parsedPort < 1 || parsedPort > 65535) {
    return DEFAULT_DEV_SERVER_PORT;
  }

  return parsedPort;
};

// https://vite.dev/config/
export default defineConfig(({ mode }) => {
  const envDir = path.resolve(__dirname, "..");
  const env = loadEnv(mode, envDir, "");
  const rawPort = env.VITE_DEV_SERVER_PORT || env.RETONE_DEV_SERVER_PORT;
  const devServerPort = parseDevServerPort(rawPort);

  return {
    plugins: [react(), tailwindcss()],
    base: "./", // Relative asset paths for the bundled Tauri frontend
    envDir, // Load .env from project root
    resolve: {
      alias: {
        "@": path.resolve(__dirname, "."),
      },
    },
    server: {
      port: devServerPort,
      strictPort: true,
      host: "127.0.0.1",
    },
    // Keep cargo output from `tauri dev` visible.
    clearScreen: false,
    build: {
      outDir: "dist",
      assetsDir: "assets",
      rolldownOptions: {
        output: {
          manualChunks(id) {
            if (
              id.includes("@radix-ui/react-dialog") ||
              id.includes("@radix-ui/react-dropdown-menu") ||
              id.includes("@radix-ui/react-select") ||
              id.includes("@radix-ui/react-tabs")
            ) {
              return "vendor-radix";
            }
          },
        },
      },
    },
  };
});
