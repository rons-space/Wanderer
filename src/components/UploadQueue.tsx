import { useState, useEffect, useCallback } from "react";
import { api } from "@/lib/api";
import { QueueItem, UploadEvent, UploadProgressEvent, QueueCounts, RateLimitEvent } from "@/types";
import { listen } from "@tauri-apps/api/event";
import { Progress } from "@/components/ui/progress";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Upload, RefreshCw, AlertCircle, CheckCircle2, Clock, Loader2, Zap, Timer } from "lucide-react";
import { toast } from "sonner";
import { formatSpeed, formatEta } from "@/lib/format";

export function UploadQueue() {
    const [items, setItems] = useState<QueueItem[]>([]);
    const [counts, setCounts] = useState<QueueCounts>({ pending: 0, uploading: 0, failed: 0 });
    const [isLoading, setIsLoading] = useState(true);
    const [currentProgress, setCurrentProgress] = useState<UploadProgressEvent | null>(null);
    const [rateLimitCountdown, setRateLimitCountdown] = useState<number | null>(null);
    // The bar has to be drawn against the wait Telegram actually asked for. A
    // FLOOD_WAIT is routinely longer than a minute, and a fixed 60s denominator
    // made the progress negative for every one of those.
    const [rateLimitTotal, setRateLimitTotal] = useState<number | null>(null);
    const [rateLimitFile, setRateLimitFile] = useState<string | null>(null);

    const loadData = useCallback(async () => {
        try {
            const [queueItems, queueCounts] = await Promise.all([
                api.getUploadQueue(),
                api.getQueueCounts(),
            ]);
            setItems(queueItems);
            setCounts(queueCounts);
        } catch (e) {
            console.error("Failed to load queue:", e);
        } finally {
            setIsLoading(false);
        }
    }, []);

    useEffect(() => {
        loadData();

        // Listen for upload events
        const unlistenStarted = listen<UploadEvent>("upload-started", () => {
            loadData();
        });

        const unlistenCompleted = listen<UploadEvent>("upload-completed", (event) => {
            toast.success(`Uploaded: ${getFileName(event.payload.filePath)}`);
            loadData();
        });

        const unlistenFailed = listen<UploadEvent>("upload-failed", (event) => {
            toast.error(`Upload failed: ${event.payload.error || "Unknown error"}`);
            setCurrentProgress(null);
            loadData();
        });

        // Listen for progress updates
        const unlistenProgress = listen<UploadProgressEvent>("upload-progress", (event) => {
            setCurrentProgress(event.payload);
        });

        // Listen for rate limit events
        const unlistenRateLimit = listen<RateLimitEvent>("upload-rate-limited", (event) => {
            const waitSecs = event.payload.waitSeconds;
            setRateLimitCountdown(waitSecs);
            setRateLimitTotal(waitSecs > 0 ? waitSecs : null);
            setRateLimitFile(event.payload.filePath.split(/[/\\]/).pop() || event.payload.filePath);
            toast.warning(`Rate limited by Telegram. Waiting ${waitSecs}s...`);
        });

        return () => {
            unlistenStarted.then((fn) => fn());
            unlistenCompleted.then((fn) => fn());
            unlistenFailed.then((fn) => fn());
            unlistenProgress.then((fn) => fn());
            unlistenRateLimit.then((fn) => fn());
        };
    }, [loadData]);

    // Countdown timer for rate limiting
    useEffect(() => {
        if (rateLimitCountdown === null || rateLimitCountdown <= 0) {
            if (rateLimitCountdown === 0) {
                setRateLimitCountdown(null);
                setRateLimitTotal(null);
                setRateLimitFile(null);
                loadData(); // Refresh after rate limit ends
            }
            return;
        }

        const timer = setInterval(() => {
            setRateLimitCountdown((prev) => (prev !== null && prev > 0 ? prev - 1 : null));
        }, 1000);

        return () => clearInterval(timer);
    }, [rateLimitCountdown, loadData]);

    const handleRetry = async (id: number) => {
        try {
            await api.retryUpload(id);
            toast.success("Retrying upload...");
            loadData();
        } catch (e) {
            console.error("Retry failed:", e);
            toast.error("Failed to retry upload");
        }
    };

    const getFileName = (path: string) => {
        return path.split(/[/\\]/).pop() || path;
    };

    const getStatusIcon = (status: string) => {
        switch (status) {
            case "pending":
                return <Clock className="h-4 w-4 text-muted-foreground" />;
            case "uploading":
                return <Loader2 className="h-4 w-4 text-blue-500 animate-spin" />;
            case "completed":
                return <CheckCircle2 className="h-4 w-4 text-green-500" />;
            case "failed":
                return <AlertCircle className="h-4 w-4 text-red-500" />;
            default:
                return null;
        }
    };

    const getStatusBadge = (status: string) => {
        const variants: Record<string, "default" | "secondary" | "destructive" | "outline"> = {
            pending: "secondary",
            uploading: "default",
            completed: "outline",
            failed: "destructive",
        };
        return (
            <Badge variant={variants[status] || "outline"} className="capitalize">
                {status}
            </Badge>
        );
    };

    const totalActive = counts.pending + counts.uploading;

    const rateLimitProgress =
        rateLimitCountdown !== null && rateLimitTotal !== null && rateLimitTotal > 0
            ? Math.min(100, Math.max(0, ((rateLimitTotal - rateLimitCountdown) / rateLimitTotal) * 100))
            : 0;

    return (
        <div className="flex flex-col h-full">
            {/* Header */}
            <div className="flex items-center justify-between p-4 border-b">
                <div className="flex items-center gap-3">
                    <div className="flex items-center justify-center w-10 h-10 rounded-full bg-blue-500/10">
                        <Upload className="w-5 h-5 text-blue-500" />
                    </div>
                    <div>
                        <h1 className="text-lg font-semibold">Upload Queue</h1>
                        <p className="text-sm text-muted-foreground">
                            {totalActive > 0
                                ? `${counts.uploading} uploading, ${counts.pending} pending`
                                : counts.failed > 0
                                    ? `${counts.failed} failed`
                                    : "All uploads complete"}
                        </p>
                    </div>
                </div>
                <Button variant="ghost" size="icon" onClick={loadData}>
                    <RefreshCw className="h-4 w-4" />
                </Button>
            </div>

            {/* Rate Limit Warning Banner */}
            {rateLimitCountdown !== null && (
                <div className="p-4 border-b bg-orange-500/10">
                    <div className="flex items-center gap-3">
                        <div className="flex items-center justify-center w-10 h-10 rounded-full bg-orange-500/20">
                            <Timer className="w-5 h-5 text-orange-500 animate-pulse" />
                        </div>
                        <div className="flex-1">
                            <h2 className="text-sm font-semibold text-orange-700 dark:text-orange-400">
                                Rate Limited by Telegram
                            </h2>
                            <p className="text-sm text-orange-600/80 dark:text-orange-300/80">
                                Waiting <span className="font-mono font-bold">{rateLimitCountdown}s</span> before retrying
                                {rateLimitFile && <span className="ml-1 text-xs">({rateLimitFile})</span>}
                            </p>
                        </div>
                        <Badge variant="outline" className="text-orange-600 border-orange-500/50">
                            <Timer className="h-3 w-3 mr-1" />
                            {rateLimitCountdown}s
                        </Badge>
                    </div>
                    <Progress value={rateLimitProgress} className="h-1 mt-2" />
                </div>
            )}

            {/* Progress Overview */}
            {totalActive > 0 && (
                <div className="p-4 border-b bg-muted/30">
                    <div className="flex items-center justify-between mb-2">
                        <span className="text-sm font-medium">Upload Progress</span>
                        <div className="flex items-center gap-2">
                            {currentProgress && (
                                <>
                                    <Badge variant="secondary" className="flex items-center gap-1">
                                        <Zap className="h-3 w-3" />
                                        {formatSpeed(currentProgress.speedBps)}
                                    </Badge>
                                    <Badge variant="outline">
                                        {formatEta(currentProgress.etaSeconds)}
                                    </Badge>
                                </>
                            )}
                            <span className="text-sm text-muted-foreground">
                                {counts.uploading} / {totalActive}
                            </span>
                        </div>
                    </div>
                    <Progress
                        value={currentProgress ? currentProgress.percent : 0}
                        className="h-2"
                    />
                    {currentProgress && (
                        <p className="text-xs text-muted-foreground mt-1">
                            {(currentProgress.bytesUploaded / (1024 * 1024)).toFixed(1)} MB /
                            {(currentProgress.totalBytes / (1024 * 1024)).toFixed(1)} MB
                        </p>
                    )}
                </div>
            )}

            {/* Queue Items */}
            <div className="flex-1 overflow-y-auto">
                {isLoading ? (
                    <div className="flex items-center justify-center h-32">
                        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
                    </div>
                ) : items.length === 0 ? (
                    <div className="flex flex-col items-center justify-center h-32 text-center">
                        <Upload className="w-12 h-12 mb-3 text-muted-foreground/30" />
                        <p className="text-sm text-muted-foreground">No items in queue</p>
                    </div>
                ) : (
                    <div className="divide-y">
                        {items.map((item) => (
                            <div
                                key={item.id}
                                className="flex items-center gap-3 p-3 hover:bg-muted/50"
                            >
                                {getStatusIcon(item.status)}
                                <div className="flex-1 min-w-0">
                                    <p className="text-sm font-medium truncate">
                                        {getFileName(item.file_path)}
                                    </p>
                                    {item.error_msg && (
                                        <p className="text-xs text-red-500 truncate">
                                            {item.error_msg}
                                        </p>
                                    )}
                                </div>
                                <div className="flex items-center gap-2">
                                    {getStatusBadge(item.status)}
                                    {item.status === "failed" && (
                                        <Button
                                            variant="ghost"
                                            size="sm"
                                            onClick={() => handleRetry(item.id)}
                                        >
                                            <RefreshCw className="h-3 w-3 mr-1" />
                                            Retry
                                        </Button>
                                    )}
                                </div>
                            </div>
                        ))}
                    </div>
                )}
            </div>

            {/* Failed Count Footer */}
            {counts.failed > 0 && (
                <div className="p-3 border-t bg-red-500/10">
                    <div className="flex items-center justify-between">
                        <div className="flex items-center gap-2 text-red-500">
                            <AlertCircle className="h-4 w-4" />
                            <span className="text-sm font-medium">
                                {counts.failed} failed upload{counts.failed > 1 ? "s" : ""}
                            </span>
                        </div>
                    </div>
                </div>
            )}
        </div>
    );
}
