import type { SyntheticEvent } from "react";

/**
 * Shown when a thumbnail is missing or fails to decode. It ships in `public/`
 * rather than being fetched, so it also works offline and under the packaged
 * CSP, where `img-src 'self'` covers it.
 */
export const PLACEHOLDER_SRC = "/placeholder.svg";

/**
 * Swap a broken image for the placeholder exactly once.
 *
 * Assigning `src` inside `onError` re-enters this handler if the replacement
 * also fails, which is an endless request loop rather than a broken image. The
 * marker on the element is what stops the second pass.
 */
export function handleImageError(event: SyntheticEvent<HTMLImageElement>): void {
    const img = event.currentTarget;

    if (img.dataset.placeholderApplied === "true") {
        return;
    }

    img.dataset.placeholderApplied = "true";
    img.src = PLACEHOLDER_SRC;
}
