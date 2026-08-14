import { useEffect, useState } from "react";
import { toast } from "sonner";
import { api, errorMessage } from "@/lib/api";
import { Alert, AlertDescription, AlertTitle } from "../ui/alert";
import { Button } from "../ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../ui/card";
import { Input } from "../ui/input";
import { Label } from "../ui/label";
import { Separator } from "../ui/separator";
import { Slider } from "../ui/slider";
import { TabsContent } from "../ui/tabs";
import { Copy, HardDrive } from "lucide-react";
import type { AppConfig } from "./use-app-config";
import type { SecurityStatus } from "./use-security-status";

interface StorageTabProps {
    config: AppConfig;
    isSaving: boolean;
    saveConfig: (key: keyof AppConfig, value: string) => Promise<void>;
    previewConfig: (key: keyof AppConfig, value: string) => void;
    securityStatus: SecurityStatus | null;
}

export function StorageTab({
    config,
    isSaving,
    saveConfig,
    previewConfig,
    securityStatus,
}: StorageTabProps) {
    const [backupPath, setBackupPath] = useState<string>("");

    useEffect(() => {
        const load = async () => {
            try {
                setBackupPath(await api.getBackupPath());
            } catch (e) {
                console.error("Failed to load backup path:", e);
                setBackupPath("");
            }
        };
        load();
    }, []);

    const cacheSizeMb = parseInt(config.cache_size_mb) || 5000;
    const viewCacheSizeMb = parseInt(config.view_cache_max_size_mb) || 2000;
    const viewCacheRetentionHours = parseInt(config.view_cache_retention_hours) || 24;

    return (
        <TabsContent value="storage" className="mt-6">
            <Card>
                <CardHeader>
                    <CardTitle>Cache Settings</CardTitle>
                    <CardDescription>Manage local storage for thumbnails and cached photos</CardDescription>
                </CardHeader>
                <CardContent className="space-y-6">
                    <div className="space-y-3">
                        <Label htmlFor="backup-path">Backup Folder</Label>
                        <div className="flex gap-2">
                            <Input
                                id="backup-path"
                                value={backupPath || "Loading..."}
                                readOnly
                                className="font-mono text-xs"
                            />
                            <Button
                                type="button"
                                variant="outline"
                                size="icon"
                                onClick={async () => {
                                    if (!backupPath) return;
                                    try {
                                        await navigator.clipboard.writeText(backupPath);
                                        toast.success("Backup path copied");
                                    } catch (e) {
                                        console.error("Failed to copy backup path:", e);
                                        toast.error("Failed to copy backup path");
                                    }
                                }}
                                disabled={!backupPath}
                            >
                                <Copy className="h-4 w-4" />
                            </Button>
                        </div>
                        <p className="text-xs text-muted-foreground">
                            Current local folder used to store imported media files.
                        </p>
                    </div>

                    <Separator />

                    <div className="space-y-4">
                        <div className="flex justify-between items-center">
                            <Label>Maximum Thumbnail Cache Size</Label>
                            <span className="text-sm font-mono bg-muted px-2 py-1 rounded">
                                {(cacheSizeMb / 1000).toFixed(1)} GB
                            </span>
                        </div>
                        <Slider
                            value={[cacheSizeMb]}
                            min={500}
                            max={50000}
                            step={500}
                            onValueChange={(value) => {
                                const next = value[0];
                                if (typeof next !== "number") return;
                                previewConfig("cache_size_mb", String(next));
                            }}
                            onValueCommit={(value) => {
                                const next = value[0];
                                if (typeof next !== "number") return;
                                saveConfig("cache_size_mb", String(next));
                            }}
                            disabled={isSaving}
                        />
                        <p className="text-xs text-muted-foreground">
                            Grid thumbnails are kept on disk so the timeline scrolls without
                            re-reading your photos. Once they pass this limit the least recently
                            used are removed at startup and regenerated when you next scroll past
                            them. Full-size previews have their own limit below.
                        </p>
                    </div>

                    <Separator />

                    <div className="space-y-4">
                        <div className="flex justify-between items-center">
                            <Label>Cloud View Cache Size</Label>
                            <span className="text-sm font-mono bg-muted px-2 py-1 rounded">
                                {(viewCacheSizeMb / 1000).toFixed(1)} GB
                            </span>
                        </div>
                        <Slider
                            value={[viewCacheSizeMb]}
                            min={100}
                            max={10000}
                            step={100}
                            onValueChange={(value) => {
                                const next = value[0];
                                if (typeof next !== "number") return;
                                previewConfig("view_cache_max_size_mb", String(next));
                            }}
                            onValueCommit={(value) => {
                                const next = value[0];
                                if (typeof next !== "number") return;
                                saveConfig("view_cache_max_size_mb", String(next));
                            }}
                            disabled={isSaving}
                        />
                        <p className="text-xs text-muted-foreground">
                            Maximum disk space for temporary copies of cloud-only files.
                        </p>
                    </div>

                    <Separator />

                    <div className="space-y-4">
                        <div className="flex justify-between items-center">
                            <Label>Cloud View Retention</Label>
                            <span className="text-sm font-mono bg-muted px-2 py-1 rounded">
                                {viewCacheRetentionHours} Hours
                            </span>
                        </div>
                        <Slider
                            value={[viewCacheRetentionHours]}
                            min={1}
                            max={168} // 1 week
                            step={1}
                            onValueChange={(value) => {
                                const next = value[0];
                                if (typeof next !== "number") return;
                                previewConfig("view_cache_retention_hours", String(next));
                            }}
                            onValueCommit={(value) => {
                                const next = value[0];
                                if (typeof next !== "number") return;
                                saveConfig("view_cache_retention_hours", String(next));
                            }}
                            disabled={isSaving}
                        />
                        <p className="text-xs text-muted-foreground">
                            Time to keep temporary copies after last view.
                        </p>
                    </div>
                </CardContent>
            </Card>


            {/* Database Backup Card */}
            <Card className="mt-4">
                <CardHeader>
                    <CardTitle>Database Backup</CardTitle>
                    <CardDescription>Backup your library metadata (albums, favorites, ratings)</CardDescription>
                </CardHeader>
                <CardContent className="space-y-4">
                    <div className="flex gap-4">
                        <Button
                            variant="outline"
                            onClick={async () => {
                                try {
                                    const { open } = await import("@tauri-apps/plugin-dialog");
                                    const folder = await open({ directory: true, multiple: false });
                                    if (folder) {
                                        const path = await api.backupDatabase(folder as string, false);
                                        toast.success(`Backup saved to: ${path}`);
                                    }
                                } catch (e) {
                                    toast.error(`Backup failed: ${errorMessage(e)}`);
                                }
                            }}
                        >
                            <HardDrive className="mr-2 h-4 w-4" />
                            Save to File
                        </Button>
                        <Button
                            variant="outline"
                            onClick={async () => {
                                try {
                                    const path = await api.backupDatabase(undefined, true);
                                    toast.success(`Backup uploaded to Telegram. Local copy: ${path}`);
                                } catch (e) {
                                    toast.error(`Backup failed: ${errorMessage(e)}`);
                                }
                            }}
                        >
                            Upload to Telegram
                        </Button>
                    </div>
                    <p className="text-xs text-muted-foreground">
                        Backups include all metadata (albums, favorites, face data) but not the actual photos.
                    </p>

                    {securityStatus?.securityMode === "encrypted" && (
                        <Alert>
                            <AlertTitle>Keep your recovery key and your library.db</AlertTitle>
                            <AlertDescription className="space-y-2">
                                <p>
                                    In encrypted mode the backup is written as a <code>.wbak</code> archive,
                                    which carries the recovery-key-wrapped master key alongside the encrypted
                                    database. You can open it on a new machine with your recovery key or your
                                    passphrase, so store one of them somewhere you will still have it if this
                                    computer is lost.
                                </p>
                                <p>
                                    Older <code>.db.wbenc</code> backups cannot be restored. They were encrypted
                                    with a key whose only copy stayed in <code>library.db</code>. If you have one,
                                    keep your current <code>library.db</code> safe and take a fresh backup now.
                                </p>
                            </AlertDescription>
                        </Alert>
                    )}
                </CardContent>
            </Card>
        </TabsContent>
    );
}
