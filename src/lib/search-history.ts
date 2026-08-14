import { SearchFilters } from "@/types";

export const SEARCH_HISTORY_KEY = "wanderer_search_history";
export const MAX_HISTORY_ITEMS = 10;

/**
 * Reads the saved queries. localStorage is shared with whatever else runs in
 * this origin and survives across versions, so the stored value is treated as
 * untrusted: anything that is not an array of strings reads as no history at
 * all rather than crashing the search view on mount.
 */
export function readSearchHistory(storage: Pick<Storage, "getItem"> = localStorage): string[] {
    try {
        const stored = storage.getItem(SEARCH_HISTORY_KEY);
        if (!stored) return [];
        const parsed: unknown = JSON.parse(stored);
        if (!Array.isArray(parsed)) return [];
        return parsed.filter((entry): entry is string => typeof entry === "string");
    } catch {
        return [];
    }
}

export function writeSearchHistory(
    history: string[],
    storage: Pick<Storage, "setItem"> = localStorage,
): void {
    storage.setItem(SEARCH_HISTORY_KEY, JSON.stringify(history.slice(0, MAX_HISTORY_ITEMS)));
}

/**
 * The query moves to the front rather than being appended, so repeating a
 * search does not push the rest of the list down by one every time. A blank
 * query is not history.
 */
export function addToHistory(history: string[], query: string): string[] {
    if (!query.trim()) {
        return history;
    }

    return [query, ...history.filter((entry) => entry !== query)].slice(0, MAX_HISTORY_ITEMS);
}

export function removeFromHistory(history: string[], query: string): string[] {
    return history.filter((entry) => entry !== query);
}

export interface FilterInputs {
    favoritesOnly: boolean;
    /** The select's value, so a string even though the filter takes a number. */
    minRating: string;
    cameraMake: string;
    /** "any" | "yes" | "no", where "any" means do not filter on location. */
    hasLocation: string;
}

/**
 * Turns the filter controls into the query the backend takes. Every neutral
 * control has to come out as `undefined` rather than as a falsy value, because
 * the backend distinguishes "no rating filter" from "rating at least 0".
 */
export function buildSearchFilters(inputs: FilterInputs): SearchFilters {
    const minRating = parseInt(inputs.minRating);

    return {
        favorites_only: inputs.favoritesOnly,
        min_rating: Number.isFinite(minRating) && minRating > 0 ? minRating : undefined,
        camera_make: inputs.cameraMake.trim() || undefined,
        has_location: inputs.hasLocation === "any" ? undefined : inputs.hasLocation === "yes",
    };
}
