import { MediaItem } from "@/types";

/** How far apart two photos have to be before the grid draws a separator. */
export type TimelineGrouping = "day" | "month" | "year";

export type SeparatorRow = {
    type: "separator";
    dateKey: string;
    label: string;
    firstItemIndex: number;
};

export type ItemsRow = {
    type: "items";
    dateKey: string;
    startIndex: number;
    count: number;
};

export type DisplayRow = SeparatorRow | ItemsRow;

const MONTH_NAMES = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/**
 * The bucket a photo falls into, in local time. Local rather than UTC because
 * the separator is answering "which day was I on holiday", which is the day the
 * camera's clock showed, not the day in Greenwich.
 */
export const getDateKey = (timestamp: number, grouping: TimelineGrouping): string => {
    const date = new Date(timestamp * 1000);
    const year = date.getFullYear().toString();
    const month = String(date.getMonth() + 1).padStart(2, "0");
    const day = String(date.getDate()).padStart(2, "0");

    switch (grouping) {
        case "year":
            return year;
        case "month":
            return `${year}-${month}`;
        case "day":
        default:
            return `${year}-${month}-${day}`;
    }
};

export const formatDateKey = (dateKey: string, grouping: TimelineGrouping): string => {
    const parts = dateKey.split("-");

    switch (grouping) {
        case "year":
            return parts[0];
        case "month":
            return `${MONTH_NAMES[parseInt(parts[1]) - 1]} ${parts[0]}`;
        case "day":
        default:
            return `${MONTH_NAMES[parseInt(parts[1]) - 1]} ${parseInt(parts[2])}, ${parts[0]}`;
    }
};

/**
 * EXIF dates arrive in two shapes. The tag itself is specified as
 * "YYYY:MM:DD HH:mm:ss", but anything that has passed through a database or a
 * JSON export tends to have been normalised to "YYYY-MM-DD HH:mm:ss" on the
 * way, so both have to parse.
 */
export const parseDateTakenToTimestamp = (dateTaken?: string): number | null => {
    if (!dateTaken) {
        return null;
    }

    const normalized = dateTaken
        .trim()
        .replace(/^(\d{4}):(\d{2}):(\d{2})/, "$1-$2-$3")
        .replace(" ", "T");
    const parsed = Date.parse(normalized);
    if (Number.isNaN(parsed)) {
        return null;
    }

    return Math.floor(parsed / 1000);
};

/**
 * When the photo was taken, falling back to when we first saw it. An import
 * of an old library would otherwise pile every photo onto today.
 */
export const getTimelineTimestamp = (item: MediaItem): number =>
    parseDateTakenToTimestamp(item.date_taken) ?? item.created_at;

/**
 * What a screen reader announces for a cell. The filename is the only thing
 * that distinguishes one photo from another to a non-sighted user, so it leads,
 * and the rest of the badges the cell draws are spelled out after it.
 */
export const describeItem = (item: MediaItem): string => {
    const name = item.file_path.split(/[\\/]/).pop() || `Media ${item.id}`;
    const kind = item.mime_type?.startsWith("video") ? "Video" : "Photo";
    const notes: string[] = [];

    if (item.is_favorite) {
        notes.push("favorite");
    }
    if (item.rating > 0) {
        notes.push(`rated ${item.rating} of 5`);
    }
    if (item.is_cloud_only) {
        notes.push("cloud only");
    }

    return notes.length > 0 ? `${kind} ${name}, ${notes.join(", ")}` : `${kind} ${name}`;
};

/**
 * Flattens the item list into the rows the virtualiser scrolls through: one
 * separator per date bucket, then that bucket's items cut into rows of
 * `columnCount`. A bucket never shares a row with the next one, which is why
 * the last row of a group can be short.
 */
export const buildDisplayRows = (
    items: MediaItem[],
    grouping: TimelineGrouping,
    columnCount: number,
): DisplayRow[] => {
    if (items.length === 0 || columnCount <= 0) {
        return [];
    }

    const rows: DisplayRow[] = [];
    let index = 0;

    while (index < items.length) {
        const dateKey = getDateKey(getTimelineTimestamp(items[index]), grouping);
        rows.push({
            type: "separator",
            dateKey,
            label: formatDateKey(dateKey, grouping),
            firstItemIndex: index,
        });

        let groupEnd = index + 1;
        while (
            groupEnd < items.length &&
            getDateKey(getTimelineTimestamp(items[groupEnd]), grouping) === dateKey
        ) {
            groupEnd += 1;
        }

        let rowStart = index;
        while (rowStart < groupEnd) {
            const count = Math.min(columnCount, groupEnd - rowStart);
            rows.push({
                type: "items",
                dateKey,
                startIndex: rowStart,
                count,
            });
            rowStart += count;
        }

        index = groupEnd;
    }

    return rows;
};

/**
 * Adds the skeleton cell that stands in for the page being fetched. It fills
 * the gap on the last row where there is one, so the grid does not visibly
 * reflow when the real items land.
 */
export const withLoadingCell = (
    rows: DisplayRow[],
    itemCount: number,
    columnCount: number,
): DisplayRow[] => {
    if (rows.length === 0) {
        return [{ type: "items", dateKey: "", startIndex: itemCount, count: 1 }];
    }

    const nextRows = [...rows];
    const lastRow = nextRows[nextRows.length - 1];

    if (lastRow.type === "items" && lastRow.count < columnCount) {
        nextRows[nextRows.length - 1] = { ...lastRow, count: lastRow.count + 1 };
        return nextRows;
    }

    nextRows.push({
        type: "items",
        dateKey: lastRow.dateKey,
        startIndex: itemCount,
        count: 1,
    });
    return nextRows;
};

/** Row heights and their running offsets, in the order the rows are drawn. */
export interface RowLayout {
    heights: number[];
    offsets: number[];
    totalHeight: number;
}

export const measureRows = (
    rows: DisplayRow[],
    separatorHeight: number,
    itemRowHeight: number,
): RowLayout => {
    const heights = rows.map((row) => (row.type === "separator" ? separatorHeight : itemRowHeight));
    const offsets: number[] = [];
    let offset = 0;

    for (const height of heights) {
        offsets.push(offset);
        offset += height;
    }

    return { heights, offsets, totalHeight: offset };
};

/**
 * The row covering a scroll offset, by binary search over the offsets. Returns
 * -1 for an empty list, and clamps into range for an offset past the end,
 * which happens during an overscroll bounce.
 */
export const findRowIndexAtOffset = (layout: RowLayout, offset: number): number => {
    const { heights, offsets } = layout;
    if (heights.length === 0) {
        return -1;
    }

    let low = 0;
    let high = heights.length - 1;

    while (low <= high) {
        const mid = Math.floor((low + high) / 2);
        const rowStart = offsets[mid];
        const rowEnd = rowStart + heights[mid];

        if (offset < rowStart) {
            high = mid - 1;
        } else if (offset >= rowEnd) {
            low = mid + 1;
        } else {
            return mid;
        }
    }

    return Math.max(0, Math.min(heights.length - 1, low));
};

/**
 * The item the floating date header should name for a given scroll position:
 * the first item at or after the topmost visible row.
 */
export const firstItemIndexFromRow = (
    rows: DisplayRow[],
    rowIndex: number,
    itemCount: number,
): number | undefined => {
    if (rowIndex < 0) {
        return undefined;
    }

    for (let index = rowIndex; index < rows.length; index += 1) {
        const row = rows[index];

        if (row.type === "separator") {
            return row.firstItemIndex;
        }

        return itemCount > 0 ? Math.min(row.startIndex, itemCount - 1) : undefined;
    }

    return undefined;
};
