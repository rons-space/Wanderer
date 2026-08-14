import js from "@eslint/js";
import globals from "globals";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import jsxA11y from "eslint-plugin-jsx-a11y";

// ESLint is pinned to 9 rather than 10 because eslint-plugin-jsx-a11y does not yet
// declare support for 10, and dropping the a11y rules to move a major version would
// give up more than it gains.
//
// The rules below are split into two groups. Everything left at `error` is clean today
// and CI fails on a regression. The rules downgraded to `warn` in the second block have
// a pre-existing backlog that belongs to tasks already scheduled in the remediation
// plan, so they are reported but do not block. `npm run lint` pins the warning count so
// the backlog cannot quietly grow while those tasks wait.
export default tseslint.config(
    {
        ignores: ["dist", "src-tauri/target", "coverage", "*.config.js"],
    },
    js.configs.recommended,
    ...tseslint.configs.recommended,
    {
        files: ["**/*.{ts,tsx}"],
        languageOptions: {
            ecmaVersion: 2022,
            globals: globals.browser,
        },
        plugins: {
            "react-hooks": reactHooks,
            "react-refresh": reactRefresh,
            "jsx-a11y": jsxA11y,
        },
        rules: {
            ...reactHooks.configs.recommended.rules,
            ...jsxA11y.flatConfigs.recommended.rules,

            // The media this app renders is the user's own photos and videos. There is
            // no caption track to point at, so the rule can only ever be noise here.
            "jsx-a11y/media-has-caption": "off",

            // Backlog rules. Each one is a real finding, each one is someone's task,
            // and none of them should hold up the pipeline that will keep the next
            // batch honest.
            //
            // react-hooks/*: the effect and lifecycle work in T48 (issue #56), with
            // the stale-response cases in T43 (issue #51).
            "react-hooks/set-state-in-effect": "warn",
            "react-hooks/immutability": "warn",
            "react-hooks/purity": "warn",
            // jsx-a11y keyboard handlers: T44 (issue #52).
            "jsx-a11y/no-static-element-interactions": "warn",
            "jsx-a11y/click-events-have-key-events": "warn",
            "jsx-a11y/no-autofocus": "warn",

            "react-refresh/only-export-components": [
                "warn",
                { allowConstantExport: true },
            ],
        },
    },
    {
        // shadcn/ui primitives are generated and re-generated from upstream. Local
        // edits to satisfy a lint are lost on the next sync, so the two rules those
        // files trip are scoped off here rather than patched in place.
        files: ["src/components/ui/**/*.{ts,tsx}"],
        rules: {
            "jsx-a11y/heading-has-content": "off",
            "react-hooks/purity": "off",
        },
    },
);
