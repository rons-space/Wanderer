import { useEffect, useState } from "react";
import { toast } from "sonner";
import { api, errorMessage } from "@/lib/api";
import { subscribe } from "@/lib/events";
import { Alert, AlertDescription, AlertTitle } from "../ui/alert";
import { Button } from "../ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../ui/card";
import { Label } from "../ui/label";
import { Progress } from "../ui/progress";
import { Separator } from "../ui/separator";
import { Switch } from "../ui/switch";
import { TabsContent } from "../ui/tabs";
import type { AppConfig } from "./use-app-config";

type ModelDownloadProgress = {
    model: string;
    current: number;
    total: number;
};

interface AiTabProps {
    config: AppConfig;
    isSaving: boolean;
    saveConfig: (key: keyof AppConfig, value: string) => Promise<void>;
}

export function AiTab({ config, isSaving, saveConfig }: AiTabProps) {
    const [clipInstalled, setClipInstalled] = useState(false);
    const [isDownloadingModels, setIsDownloadingModels] = useState(false);
    const [downloadProgress, setDownloadProgress] = useState<ModelDownloadProgress | null>(null);

    useEffect(() => {
        const checkClipStatus = async () => {
            try {
                setClipInstalled(await api.checkClipModels());
            } catch (e) {
                console.error("Failed to check CLIP status:", e);
            }
        };
        checkClipStatus();

        return subscribe<ModelDownloadProgress>("model_download_progress", (event) => {
            setDownloadProgress(event.payload);
        });
    }, []);

    const aiFaceEnabled = config.ai_face_enabled === "true";
    const aiTagsEnabled = config.ai_tags_enabled === "true";

    return (
        <TabsContent value="ai" className="mt-6 space-y-4">
            <Card>
                <CardHeader>
                    <CardTitle>AI Features</CardTitle>
                    <CardDescription>All AI processing happens locally on your device</CardDescription>
                </CardHeader>
                <CardContent className="space-y-6">
                    <div className="flex items-center justify-between">
                        <div className="space-y-0.5">
                            <Label>Face Detection</Label>
                            <p className="text-xs text-muted-foreground">
                                Detect faces in photos and group by person
                            </p>
                        </div>
                        <Switch
                            checked={aiFaceEnabled}
                            onCheckedChange={(checked) => saveConfig("ai_face_enabled", String(checked))}
                            disabled={isSaving}
                        />
                    </div>

                    <Separator />

                    <div className="flex items-center justify-between">
                        <div className="space-y-0.5">
                            <Label>Object/Scene Tagging</Label>
                            <p className="text-xs text-muted-foreground">
                                Automatically tag photos with detected objects and scenes
                            </p>
                        </div>
                        <Switch
                            checked={aiTagsEnabled}
                            onCheckedChange={(checked) => saveConfig("ai_tags_enabled", String(checked))}
                            disabled={isSaving}
                        />
                    </div>

                    <Alert>
                        <AlertTitle>🔒 Privacy First</AlertTitle>
                        <AlertDescription>
                            All AI models run locally using ONNX. Your photos never leave your device.
                        </AlertDescription>
                    </Alert>
                </CardContent>
            </Card>

            {/* CLIP Semantic Search Card */}
            <Card>
                <CardHeader>
                    <CardTitle>AI Semantic Search</CardTitle>
                    <CardDescription>Search photos using natural language (e.g., "sunset at the beach")</CardDescription>
                </CardHeader>
                <CardContent className="space-y-6">
                    <div className="flex items-center justify-between">
                        <div className="space-y-0.5">
                            <Label>Enable Semantic Search</Label>
                            <p className="text-xs text-muted-foreground">
                                Uses OpenAI's CLIP model (ViT-B/32) to understand image content
                            </p>
                        </div>
                        <Switch
                            checked={clipInstalled || isDownloadingModels}
                            onCheckedChange={async (checked) => {
                                if (checked) {
                                    if (!clipInstalled) {
                                        setIsDownloadingModels(true);
                                        try {
                                            await api.downloadClipModels();
                                            setClipInstalled(true);
                                            toast.success("CLIP models installed successfully!");
                                        } catch (e) {
                                            toast.error(`Download failed: ${errorMessage(e)}`);
                                        } finally {
                                            setIsDownloadingModels(false);
                                            setDownloadProgress(null);
                                        }
                                    }
                                } else {
                                    // For now, we don't support "uninstalling" via UI
                                    toast.info("To disable, you can delete the 'models' folder in AppData.");
                                }
                            }}
                            disabled={isDownloadingModels || isSaving}
                        />
                    </div>

                    {isDownloadingModels && downloadProgress && (
                        <div className="space-y-2">
                            <div className="flex justify-between text-xs text-muted-foreground">
                                <span>Downloading {downloadProgress.model}...</span>
                                <span>{Math.round((downloadProgress.current / downloadProgress.total) * 100)}%</span>
                            </div>
                            <Progress value={(downloadProgress.current / downloadProgress.total) * 100} />
                            <p className="text-xs text-muted-foreground text-center">
                                Downloading models (~415MB). This works offline once complete.
                            </p>
                        </div>
                    )}

                    {!isDownloadingModels && !clipInstalled && (
                        <Alert>
                            <AlertTitle>📥 Download Required</AlertTitle>
                            <AlertDescription>
                                Enabling this feature will download ~415MB of AI models to your device.
                            </AlertDescription>
                        </Alert>
                    )}

                    {clipInstalled && !isDownloadingModels && (
                        <div className="pt-2">
                            <Button
                                variant="secondary"
                                className="w-full"
                                onClick={async () => {
                                    try {
                                        toast.loading("Indexing pending images...");
                                        const count = await api.indexPendingClip(50);
                                        toast.dismiss();
                                        if (count > 0) {
                                            toast.success(`Successfully indexed ${count} new images for search!`);
                                        } else {
                                            toast.info("No new images to index.");
                                        }
                                    } catch (e) {
                                        toast.dismiss();
                                        toast.error(`Indexing failed: ${errorMessage(e)}`);
                                    }
                                }}
                            >
                                Process 50 Pending Images
                            </Button>
                            <p className="text-xs text-muted-foreground mt-2 text-center">
                                Processing happens automatically in the background, but you can force it here.
                            </p>
                        </div>
                    )}

                    {clipInstalled && (
                        <Alert className="bg-green-50 border-green-200">
                            <AlertTitle className="text-green-800">✓ Active</AlertTitle>
                            <AlertDescription className="text-green-700">
                                Semantic search is ready. Try searching for "dog", "mountain", or "wedding".
                            </AlertDescription>
                        </Alert>
                    )}
                </CardContent>
            </Card>

            {/* Multi-Device Sync Card */}
            <Card>
                <CardHeader>
                    <CardTitle>Multi-Device Sync</CardTitle>
                    <CardDescription>Sync favorites, ratings, and albums across devices</CardDescription>
                </CardHeader>
                <CardContent className="space-y-4">
                    <div className="flex gap-2">
                        <Button
                            variant="outline"
                            onClick={async () => {
                                try {
                                    const path = await api.exportSyncManifest();
                                    toast.success(`Sync manifest exported to: ${path}`);
                                } catch (e) {
                                    toast.error(`Export failed: ${errorMessage(e)}`);
                                }
                            }}
                        >
                            Export Sync Manifest
                        </Button>
                        <Button
                            variant="outline"
                            onClick={async () => {
                                try {
                                    const { open } = await import("@tauri-apps/plugin-dialog");
                                    const selected = await open({
                                        title: "Select Sync Manifest",
                                        filters: [{ name: "JSON", extensions: ["json"] }],
                                    });
                                    if (selected) {
                                        const result = await api.importSyncManifest(selected as string);
                                        toast.success(result);
                                    }
                                } catch (e) {
                                    toast.error(`Import failed: ${errorMessage(e)}`);
                                }
                            }}
                        >
                            Import Sync Manifest
                        </Button>
                    </div>
                    <p className="text-xs text-muted-foreground">
                        Export your library metadata, then import on another device to sync favorites, ratings, and albums.
                        Uses Last-Write-Wins (LWW) for conflict resolution.
                    </p>
                </CardContent>
            </Card>
        </TabsContent>
    );
}
