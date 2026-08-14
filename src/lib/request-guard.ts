/**
 * Sequencing for overlapping async requests, with no React in it so it can be
 * tested directly.
 *
 * `begin()` claims the newest slot and returns a predicate that stays true only
 * while nothing newer has been started and the guard has not been retired.
 */
export interface RequestGuard {
    begin: () => () => boolean;
    retire: () => void;
}

export function createRequestGuard(): RequestGuard {
    let latest = 0;
    let live = true;

    return {
        begin: () => {
            const id = ++latest;
            return () => live && latest === id;
        },
        retire: () => {
            live = false;
        },
    };
}
