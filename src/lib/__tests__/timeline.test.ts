import { describe, expect, it } from "vitest";
import { MediaItem } from "@/types";
import {
    buildDisplayRows,
    describeItem,
    findRowIndexAtOffset,
    firstItemIndexFromRow,
    formatDateKey,
    getDateKey,
    getTimelineTimestamp,
    measureRows,
    parseDateTakenToTimestamp,
    withLoadingCell,
    type DisplayRow,
} from "@/lib/timeline";

/** Local midnight, so the assertions do not depend on the runner's timezone. */
const atLocal = (year: number, month: number, day: number, hour = 12): number =>
    Math.floor(new Date(year, month - 1, day, hour).getTime() / 1000);

const item = (overrides: Partial<MediaItem> & { id: number }): MediaItem =>
    ({
        file_path: `/photos/IMG_${overrides.id}.jpg`,
        file_name: `IMG_${overrides.id}.jpg`,
        mime_type: "image/jpeg",
        created_at: atLocal(2024, 5, 1),
        rating: 0,
        is_favorite: false,
        is_archived: false,
        is_deleted: false,
        is_cloud_only: false,
        ...overrides,
    }) as MediaItem;

describe("parseDateTakenToTimestamp", () => {
    it("reads the colon-separated form EXIF actually specifies", () => {
        expect(parseDateTakenToTimestamp("2023:07:14 09:30:00")).toBe(atLocal(2023, 7, 14, 9) + 30 * 60);
    });

    it("reads the dash-separated form a database round trip produces", () => {
        expect(parseDateTakenToTimestamp("2023-07-14 09:30:00")).toBe(atLocal(2023, 7, 14, 9) + 30 * 60);
    });

    it("ignores surrounding whitespace", () => {
        expect(parseDateTakenToTimestamp("  2023:07:14 09:30:00  ")).toBe(
            parseDateTakenToTimestamp("2023:07:14 09:30:00"),
        );
    });

    it("returns null rather than NaN for a missing or unparseable value", () => {
        expect(parseDateTakenToTimestamp(undefined)).toBeNull();
        expect(parseDateTakenToTimestamp("")).toBeNull();
        expect(parseDateTakenToTimestamp("last tuesday")).toBeNull();
        expect(parseDateTakenToTimestamp("0000:00:00 00:00:00")).toBeNull();
    });
});

describe("getTimelineTimestamp", () => {
    it("prefers the capture time", () => {
        const media = item({ id: 1, date_taken: "2020:01:02 03:04:05", created_at: atLocal(2024, 5, 1) });
        expect(getTimelineTimestamp(media)).toBe(parseDateTakenToTimestamp("2020:01:02 03:04:05"));
    });

    it("falls back to the import time when the capture time is missing or junk", () => {
        const imported = atLocal(2024, 5, 1);
        expect(getTimelineTimestamp(item({ id: 1, created_at: imported }))).toBe(imported);
        expect(getTimelineTimestamp(item({ id: 2, date_taken: "nonsense", created_at: imported }))).toBe(
            imported,
        );
    });
});

describe("getDateKey and formatDateKey", () => {
    const timestamp = atLocal(2023, 3, 9);

    it("buckets by the requested granularity", () => {
        expect(getDateKey(timestamp, "day")).toBe("2023-03-09");
        expect(getDateKey(timestamp, "month")).toBe("2023-03");
        expect(getDateKey(timestamp, "year")).toBe("2023");
    });

    it("zero-pads so keys sort as text in the same order as in time", () => {
        expect(getDateKey(atLocal(2023, 11, 20), "day") > getDateKey(atLocal(2023, 3, 9), "day")).toBe(true);
    });

    it("renders each granularity for a human", () => {
        expect(formatDateKey("2023-03-09", "day")).toBe("March 9, 2023");
        expect(formatDateKey("2023-03", "month")).toBe("March 2023");
        expect(formatDateKey("2023", "year")).toBe("2023");
    });
});

describe("describeItem", () => {
    it("leads with the filename, which is what tells two photos apart", () => {
        expect(describeItem(item({ id: 1, file_path: "/photos/beach.jpg" }))).toBe("Photo beach.jpg");
    });

    it("handles a Windows path", () => {
        expect(describeItem(item({ id: 1, file_path: "C:\\Users\\ron\\beach.jpg" }))).toBe("Photo beach.jpg");
    });

    it("calls a video a video", () => {
        expect(describeItem(item({ id: 1, file_path: "/v/clip.mp4", mime_type: "video/mp4" }))).toBe(
            "Video clip.mp4",
        );
    });

    it("spells out the badges the cell draws", () => {
        const described = describeItem(
            item({ id: 1, file_path: "/photos/a.jpg", is_favorite: true, rating: 4, is_cloud_only: true }),
        );
        expect(described).toBe("Photo a.jpg, favorite, rated 4 of 5, cloud only");
    });

    it("falls back to the id when the path has no filename", () => {
        expect(describeItem(item({ id: 7, file_path: "" }))).toBe("Photo Media 7");
    });
});

describe("buildDisplayRows", () => {
    const day = (d: number, id: number) => item({ id, created_at: atLocal(2024, 5, d) });

    it("returns nothing for an empty list or a zero-column grid", () => {
        expect(buildDisplayRows([], "day", 4)).toEqual([]);
        expect(buildDisplayRows([day(1, 1)], "day", 0)).toEqual([]);
    });

    it("puts a separator in front of each date bucket", () => {
        const rows = buildDisplayRows([day(1, 1), day(1, 2), day(2, 3)], "day", 4);
        expect(rows.map((row) => row.type)).toEqual(["separator", "items", "separator", "items"]);
        expect(rows.filter((row) => row.type === "separator").map((row) => row.firstItemIndex)).toEqual([
            0, 2,
        ]);
    });

    it("never lets two buckets share a row, so the last row of a group runs short", () => {
        const rows = buildDisplayRows([day(1, 1), day(1, 2), day(1, 3), day(2, 4)], "day", 2);
        const itemRows = rows.filter((row): row is Extract<DisplayRow, { type: "items" }> =>
            row.type === "items",
        );
        expect(itemRows.map((row) => [row.startIndex, row.count])).toEqual([
            [0, 2],
            [2, 1],
            [3, 1],
        ]);
    });

    it("covers every item exactly once", () => {
        const items = Array.from({ length: 17 }, (_, index) => day((index % 3) + 1, index));
        const rows = buildDisplayRows(items, "day", 4);
        const covered = rows
            .filter((row): row is Extract<DisplayRow, { type: "items" }> => row.type === "items")
            .flatMap((row) => Array.from({ length: row.count }, (_, offset) => row.startIndex + offset));
        expect(covered).toEqual(items.map((_, index) => index));
    });

    it("collapses the buckets when the grouping widens", () => {
        const items = [day(1, 1), day(2, 2), day(3, 3)];
        expect(buildDisplayRows(items, "day", 4).filter((row) => row.type === "separator")).toHaveLength(3);
        expect(buildDisplayRows(items, "month", 4).filter((row) => row.type === "separator")).toHaveLength(1);
    });
});

describe("withLoadingCell", () => {
    it("produces a single skeleton row when nothing is loaded yet", () => {
        expect(withLoadingCell([], 0, 4)).toEqual([
            { type: "items", dateKey: "", startIndex: 0, count: 1 },
        ]);
    });

    it("fills the gap on a short last row instead of adding one", () => {
        const rows: DisplayRow[] = [{ type: "items", dateKey: "2024-05-01", startIndex: 0, count: 3 }];
        expect(withLoadingCell(rows, 3, 4)).toEqual([
            { type: "items", dateKey: "2024-05-01", startIndex: 0, count: 4 },
        ]);
    });

    it("adds a row when the last one is full", () => {
        const rows: DisplayRow[] = [{ type: "items", dateKey: "2024-05-01", startIndex: 0, count: 4 }];
        expect(withLoadingCell(rows, 4, 4)).toHaveLength(2);
    });

    it("does not mutate the rows it was given", () => {
        const rows: DisplayRow[] = [{ type: "items", dateKey: "2024-05-01", startIndex: 0, count: 3 }];
        withLoadingCell(rows, 3, 4);
        expect(rows[0]).toEqual({ type: "items", dateKey: "2024-05-01", startIndex: 0, count: 3 });
    });
});

describe("measureRows and findRowIndexAtOffset", () => {
    const rows: DisplayRow[] = [
        { type: "separator", dateKey: "2024-05-01", label: "May 1, 2024", firstItemIndex: 0 },
        { type: "items", dateKey: "2024-05-01", startIndex: 0, count: 4 },
        { type: "items", dateKey: "2024-05-01", startIndex: 4, count: 2 },
    ];
    const layout = measureRows(rows, 36, 200);

    it("gives separators and item rows their own heights", () => {
        expect(layout.heights).toEqual([36, 200, 200]);
        expect(layout.offsets).toEqual([0, 36, 236]);
        expect(layout.totalHeight).toBe(436);
    });

    it("finds the row covering an offset, including at each boundary", () => {
        expect(findRowIndexAtOffset(layout, 0)).toBe(0);
        expect(findRowIndexAtOffset(layout, 35)).toBe(0);
        expect(findRowIndexAtOffset(layout, 36)).toBe(1);
        expect(findRowIndexAtOffset(layout, 235)).toBe(1);
        expect(findRowIndexAtOffset(layout, 236)).toBe(2);
    });

    it("agrees with a linear scan at every offset", () => {
        for (let offset = 0; offset < layout.totalHeight; offset += 1) {
            const expected = layout.heights.findIndex(
                (height, index) => offset >= layout.offsets[index] && offset < layout.offsets[index] + height,
            );
            expect(findRowIndexAtOffset(layout, offset)).toBe(expected);
        }
    });

    it("clamps an overscroll bounce into range instead of running off the end", () => {
        expect(findRowIndexAtOffset(layout, layout.totalHeight + 500)).toBe(2);
        expect(findRowIndexAtOffset(layout, -50)).toBe(0);
    });

    it("reports -1 when there are no rows", () => {
        expect(findRowIndexAtOffset(measureRows([], 36, 200), 0)).toBe(-1);
    });
});

describe("firstItemIndexFromRow", () => {
    const rows: DisplayRow[] = [
        { type: "separator", dateKey: "2024-05-01", label: "May 1, 2024", firstItemIndex: 0 },
        { type: "items", dateKey: "2024-05-01", startIndex: 0, count: 4 },
        { type: "separator", dateKey: "2024-05-02", label: "May 2, 2024", firstItemIndex: 4 },
        { type: "items", dateKey: "2024-05-02", startIndex: 4, count: 2 },
    ];

    it("takes the first item of a separator row", () => {
        expect(firstItemIndexFromRow(rows, 2, 6)).toBe(4);
    });

    it("takes the first item of an item row", () => {
        expect(firstItemIndexFromRow(rows, 3, 6)).toBe(4);
    });

    it("clamps to the loaded items, since the skeleton row points past them", () => {
        expect(firstItemIndexFromRow(rows, 3, 5)).toBe(4);
        expect(firstItemIndexFromRow(rows, 1, 2)).toBe(0);
    });

    it("has no answer for an empty grid or a missing row", () => {
        expect(firstItemIndexFromRow(rows, -1, 6)).toBeUndefined();
        expect(firstItemIndexFromRow([], 0, 0)).toBeUndefined();
        expect(firstItemIndexFromRow(rows, 3, 0)).toBeUndefined();
    });
});
