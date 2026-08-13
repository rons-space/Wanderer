import { describe, it, expect } from "vitest"
import {
    formatBytes,
    formatSpeed,
    formatEta,
    getFileNameFromPath,
    getFileTypeFromPath,
} from "@/lib/format"

describe("formatBytes", () => {
    it("returns a fallback for missing or non-positive input", () => {
        expect(formatBytes(undefined)).toBe("Unknown size")
        expect(formatBytes(0)).toBe("Unknown size")
        expect(formatBytes(-5)).toBe("Unknown size")
    })

    it("formats bytes with no decimals", () => {
        expect(formatBytes(512)).toBe("512 B")
        expect(formatBytes(1023)).toBe("1023 B")
    })

    it("formats KB/MB/GB with one decimal", () => {
        expect(formatBytes(1024)).toBe("1.0 KB")
        expect(formatBytes(1536)).toBe("1.5 KB")
        expect(formatBytes(1024 * 1024)).toBe("1.0 MB")
        expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MB")
        expect(formatBytes(1024 * 1024 * 1024)).toBe("1.0 GB")
    })

    it("caps the unit at TB and does not overflow the unit table", () => {
        expect(formatBytes(1024 ** 4)).toBe("1.0 TB")
        // 2048 TB stays in TB rather than inventing a PB unit
        expect(formatBytes(2048 * 1024 ** 4)).toBe("2048.0 TB")
    })
})

describe("formatSpeed", () => {
    it("formats sub-KB rates in B/s with no decimals", () => {
        expect(formatSpeed(0)).toBe("0 B/s")
        expect(formatSpeed(999)).toBe("999 B/s")
    })

    it("formats KB/s and MB/s with one decimal", () => {
        expect(formatSpeed(1024)).toBe("1.0 KB/s")
        expect(formatSpeed(1536)).toBe("1.5 KB/s")
        expect(formatSpeed(1024 * 1024)).toBe("1.0 MB/s")
        expect(formatSpeed(2.5 * 1024 * 1024)).toBe("2.5 MB/s")
    })

    it("uses the boundary at exactly 1024", () => {
        expect(formatSpeed(1023)).toBe("1023 B/s")
        expect(formatSpeed(1024)).toBe("1.0 KB/s")
    })
})

describe("formatEta", () => {
    it("formats sub-minute durations in seconds", () => {
        expect(formatEta(0)).toBe("~0s")
        expect(formatEta(59)).toBe("~59s")
    })

    it("rounds minutes up", () => {
        expect(formatEta(60)).toBe("~1 min")
        expect(formatEta(61)).toBe("~2 min")
        expect(formatEta(3599)).toBe("~60 min")
    })

    it("formats hours with one decimal at the 3600s boundary", () => {
        expect(formatEta(3600)).toBe("~1.0 hr")
        expect(formatEta(5400)).toBe("~1.5 hr")
    })
})

describe("getFileNameFromPath", () => {
    it("extracts the file name from a POSIX path", () => {
        expect(getFileNameFromPath("/home/user/photos/img.jpg")).toBe("img.jpg")
    })

    it("extracts the file name from a Windows path", () => {
        expect(getFileNameFromPath("C:\\Users\\me\\Pictures\\img.png")).toBe("img.png")
    })

    it("returns the input unchanged when there is no separator", () => {
        expect(getFileNameFromPath("img.jpg")).toBe("img.jpg")
    })

    it("handles mixed separators", () => {
        expect(getFileNameFromPath("C:/Users/me\\pics/a.heic")).toBe("a.heic")
    })
})

describe("getFileTypeFromPath", () => {
    it("prefers the MIME subtype, uppercased", () => {
        expect(getFileTypeFromPath("/x/a.png", "image/png")).toBe("PNG")
        expect(getFileTypeFromPath("/x/a.webp", "image/webp")).toBe("WEBP")
    })

    it("normalizes JPEG to JPG", () => {
        expect(getFileTypeFromPath("/x/a.jpg", "image/jpeg")).toBe("JPG")
    })

    it("falls back to the extension when no MIME type is given", () => {
        expect(getFileTypeFromPath("/x/photo.CR2")).toBe("CR2")
        expect(getFileTypeFromPath("C:\\x\\movie.mp4")).toBe("MP4")
    })

    it("returns UNKNOWN when there is no extension and no MIME", () => {
        expect(getFileTypeFromPath("/x/README")).toBe("README")
        expect(getFileTypeFromPath("")).toBe("UNKNOWN")
    })
})
