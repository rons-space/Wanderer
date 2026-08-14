import { defineConfig } from "vitest/config"
import react from "@vitejs/plugin-react"
import path from "path"

export default defineConfig({
    plugins: [react()],
    resolve: {
        alias: {
            "@": path.resolve(__dirname, "./src"),
        },
    },
    test: {
        // jsdom rather than node because the hooks under test are the ones that
        // own fetching and pagination for every media view, and they can only be
        // exercised by actually rendering them. The pure-function suites do not
        // care either way.
        environment: "jsdom",
        globals: true,
        setupFiles: ["src/test/setup.ts"],
        include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    },
})
