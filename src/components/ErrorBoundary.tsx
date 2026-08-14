import { Component, ErrorInfo, ReactNode } from "react";
import { api } from "@/lib/api";
import { Button } from "./ui/button";

interface Props {
    children?: ReactNode;
    /**
     * Names the part of the app that failed, both in the log and in the
     * fallback. Boundaries around a single view use it so that a crash there
     * says which view, rather than reading as though the whole app died.
     */
    context?: string;
    /**
     * Renders the compact fallback that fits inside a view, instead of the
     * full-screen one used at the root.
     */
    inline?: boolean;
}

interface State {
    hasError: boolean;
    error: Error | null;
}

/**
 * Catches a render crash and reports it somewhere a user can find.
 *
 * The previous version called `console.error`, which in a packaged desktop
 * build goes to a devtools console nobody has open, and offered no way out of
 * the error state short of killing the process. It now writes through to the
 * backend log and offers a reload.
 */
export class ErrorBoundary extends Component<Props, State> {
    public state: State = {
        hasError: false,
        error: null,
    };

    public static getDerivedStateFromError(error: Error): State {
        return { hasError: true, error };
    }

    public componentDidCatch(error: Error, errorInfo: ErrorInfo) {
        console.error("Uncaught error:", error, errorInfo);
        void api.reportFrontendError(
            this.props.context ?? "root",
            error.message || String(error),
            // The component stack says which tree failed, which the JS stack
            // alone usually does not after minification.
            `${error.stack ?? ""}\n${errorInfo.componentStack ?? ""}`.trim(),
        );
    }

    private handleReload = () => {
        window.location.reload();
    };

    private handleDismiss = () => {
        this.setState({ hasError: false, error: null });
    };

    public render() {
        if (!this.state.hasError) {
            return this.props.children;
        }

        const what = this.props.context ? `${this.props.context} could not be displayed` : "Something went wrong";

        if (this.props.inline) {
            return (
                <div className="flex h-full flex-col items-center justify-center gap-3 p-6 text-center">
                    <p className="text-destructive font-medium">{what}</p>
                    <pre className="bg-muted max-w-full overflow-auto rounded p-3 text-xs">
                        {this.state.error?.message || String(this.state.error)}
                    </pre>
                    <div className="flex gap-2">
                        <Button variant="outline" onClick={this.handleDismiss}>
                            Try again
                        </Button>
                        <Button onClick={this.handleReload}>Reload</Button>
                    </div>
                </div>
            );
        }

        return (
            <div className="bg-background text-foreground flex min-h-screen flex-col items-center justify-center gap-4 p-4">
                <h1 className="text-destructive text-2xl font-bold">{what}</h1>
                <pre className="bg-muted max-w-full overflow-auto rounded p-4 text-sm">
                    {this.state.error?.toString()}
                </pre>
                <p className="text-muted-foreground max-w-prose text-center text-sm">
                    The details have been written to the application log. Settings shows where to find it once
                    the app is running again.
                </p>
                <Button onClick={this.handleReload}>Reload the app</Button>
            </div>
        );
    }
}
