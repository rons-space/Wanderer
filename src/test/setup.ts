import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";

// Unmounting between tests matters more than usual here: the hooks under test
// keep request generations and timers alive, and a component left mounted from
// an earlier test goes on writing state during the next one.
afterEach(() => {
    cleanup();
});
