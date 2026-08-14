import { useState, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";
import { PLACEHOLDER_SRC, handleImageError } from "@/lib/placeholder";
import { api } from "@/lib/api";
import { MediaItem } from "@/types";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Badge } from "@/components/ui/badge";
import { Progress } from "@/components/ui/progress";
import { toast } from "sonner";
import { Trash2, Check, Copy, RefreshCw, Scan } from "lucide-react";
import { formatBytes, getFileNameFromPath, getFileTypeFromPath } from "@/lib/format";

interface DuplicateGroup {
    items: MediaItem[];
    keepIndex: number;
}

const getPreviewSrc = (item: MediaItem): string => {
    if (item.thumbnail_path) {
        return convertFileSrc(item.thumbnail_path);
    }
    if (item.file_path) {
        return convertFileSrc(item.file_path);
    }
    return PLACEHOLDER_SRC;
};

const getFileType = (item: MediaItem): string => getFileTypeFromPath(item.file_path, item.mime_type);

const getFileName = (item: MediaItem): string => getFileNameFromPath(item.file_path);

export function DuplicateReview() {
    const [groups, setGroups] = useState<DuplicateGroup[]>([]);
    const [isLoading, setIsLoading] = useState(true);
    const [processingGroup, setProcessingGroup] = useState<number | null>(null);
    const [isScanning, setIsScanning] = useState(false);
    const [scanProgress, setScanProgress] = useState({ current: 0, total: 0 });

    const loadDuplicates = async () => {
        setIsLoading(true);
        try {
            const rawGroups = await api.findDuplicates();
            const mapped = rawGroups.map((items) => ({
                items,
                keepIndex: 0, // Default to keeping the first (oldest)
            }));
            setGroups(mapped);
        } catch (e) {
            console.error("Failed to load duplicates:", e);
            toast.error("Failed to load duplicates");
        } finally {
            setIsLoading(false);
        }
    };

    useEffect(() => {
        loadDuplicates();

        // Listen for scan progress events
        const unlistenProgress = listen<[number, number]>("scan-duplicates-progress", (event) => {
            setScanProgress({ current: event.payload[0], total: event.payload[1] });
        });

        const unlistenFinished = listen<number>("scan-duplicates-finished", (event) => {
            setIsScanning(false);
            if (event.payload > 0) {
                toast.success(`Scanned ${event.payload} images`);
                loadDuplicates(); // Reload to find new duplicates
            }
        });

        return () => {
            unlistenProgress.then(fn => fn());
            unlistenFinished.then(fn => fn());
        };
    }, []);

    const handleScanLibrary = async () => {
        setIsScanning(true);
        setScanProgress({ current: 0, total: 0 });
        try {
            const count = await api.scanDuplicates();
            if (count === 0) {
                toast.info("All images already scanned");
                setIsScanning(false);
            }
        } catch (e) {
            console.error("Failed to scan library:", e);
            toast.error("Failed to scan library");
            setIsScanning(false);
        }
    };

    const handleKeepSelect = (groupIndex: number, itemIndex: number) => {
        setGroups((prev) =>
            prev.map((g, i) =>
                i === groupIndex ? { ...g, keepIndex: itemIndex } : g
            )
        );
    };

    const handleDeleteDuplicates = async (groupIndex: number) => {
        const group = groups[groupIndex];
        const toDelete = group.items
            .filter((_, i) => i !== group.keepIndex)
            .map((item) => item.id);

        setProcessingGroup(groupIndex);
        try {
            await api.bulkDelete(toDelete);
            toast.success(`Moved ${toDelete.length} duplicate(s) to trash`);
            setGroups((prev) => prev.filter((_, i) => i !== groupIndex));
        } catch (e) {
            console.error("Failed to delete duplicates:", e);
            toast.error("Failed to delete duplicates");
        } finally {
            setProcessingGroup(null);
        }
    };

    if (isLoading) {
        return (
            <div className="h-full w-full flex items-center justify-center">
                <div className="text-muted-foreground">Scanning for duplicates...</div>
            </div>
        );
    }

    if (groups.length === 0) {
        return (
            <div className="h-full w-full flex flex-col items-center justify-center gap-4">
                <div className="text-4xl">✨</div>
                <h2 className="text-xl font-semibold">No duplicates found</h2>
                <p className="text-muted-foreground text-center max-w-md">
                    Duplicates are detected using perceptual hashing which finds
                    visually similar images. Click "Scan Library" to analyze your photos.
                </p>
                {isScanning ? (
                    <div className="w-64 space-y-2">
                        <Progress value={(scanProgress.current / Math.max(scanProgress.total, 1)) * 100} />
                        <p className="text-sm text-center text-muted-foreground">
                            Scanning {scanProgress.current} of {scanProgress.total}
                        </p>
                    </div>
                ) : (
                    <div className="flex gap-2">
                        <Button variant="default" onClick={handleScanLibrary}>
                            <Scan className="h-4 w-4 mr-2" />
                            Scan Library
                        </Button>
                        <Button variant="outline" onClick={loadDuplicates}>
                            <RefreshCw className="h-4 w-4 mr-2" />
                            Refresh
                        </Button>
                    </div>
                )}
            </div>
        );
    }

    return (
        <div className="h-full w-full flex flex-col">
            {/* Header */}
            <div className="flex items-center justify-between p-4 border-b">
                <div>
                    <h1 className="text-lg font-semibold">Duplicate Review</h1>
                    <p className="text-sm text-muted-foreground">
                        Found {groups.length} group(s) of potential duplicates
                    </p>
                </div>
                <Button variant="outline" size="sm" onClick={loadDuplicates}>
                    <RefreshCw className="h-4 w-4 mr-2" />
                    Rescan
                </Button>
            </div>

            {/* Duplicate Groups */}
            <ScrollArea className="flex-1 p-4">
                <div className="space-y-6">
                    {groups.map((group, groupIndex) => (
                        <Card key={groupIndex}>
                            <CardHeader className="py-3">
                                <CardTitle className="text-sm flex items-center gap-2">
                                    <Copy className="h-4 w-4" />
                                    {group.items.length} similar photos
                                </CardTitle>
                            </CardHeader>
                            <CardContent className="py-0">
                                <div className="flex gap-3 overflow-x-auto pb-3">
                                    {group.items.map((item, itemIndex) => (
                                        <div
                                            key={item.id}
                                            className={`relative flex-shrink-0 cursor-pointer rounded-lg overflow-hidden border-2 transition-all ${itemIndex === group.keepIndex
                                                ? "border-green-500 ring-2 ring-green-500/30"
                                                : "border-transparent hover:border-muted-foreground"
                                                }`}
                                            onClick={() => handleKeepSelect(groupIndex, itemIndex)}
                                        >
                                            <img
                                                src={getPreviewSrc(item)}
                                                alt=""
                                                className="w-32 h-32 object-cover"
                                                onError={handleImageError}
                                            />
                                            {itemIndex === group.keepIndex ? (
                                                <Badge className="absolute top-1 right-1 bg-green-600">
                                                    <Check className="h-3 w-3 mr-1" />
                                                    Keep
                                                </Badge>
                                            ) : (
                                                <Badge variant="destructive" className="absolute top-1 right-1">
                                                    <Trash2 className="h-3 w-3" />
                                                </Badge>
                                            )}
                                            <div className="absolute bottom-0 left-0 right-0 bg-black/70 text-white text-xs p-1 truncate">
                                                <div className="truncate text-[11px] font-medium text-white/95 leading-tight">
                                                    {getFileType(item)} • {formatBytes(item.size_bytes)}
                                                </div>
                                                <div className="truncate text-[11px] text-white/85 leading-tight">
                                                    {getFileName(item)}
                                                </div>
                                            </div>
                                        </div>
                                    ))}
                                </div>
                            </CardContent>
                            <CardFooter className="py-3 border-t">
                                <Button
                                    size="sm"
                                    variant="destructive"
                                    onClick={() => handleDeleteDuplicates(groupIndex)}
                                    disabled={processingGroup === groupIndex}
                                >
                                    <Trash2 className="h-4 w-4 mr-2" />
                                    Delete {group.items.length - 1} duplicate(s)
                                </Button>
                            </CardFooter>
                        </Card>
                    ))}
                </div>
            </ScrollArea>
        </div>
    );
}
