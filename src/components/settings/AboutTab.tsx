import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { toast } from "sonner";
import { api } from "@/lib/api";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../ui/card";
import { Button } from "../ui/button";
import { Label } from "../ui/label";
import { Separator } from "../ui/separator";
import { TabsContent } from "../ui/tabs";
import { Copy, ExternalLink, Github, HandHeart, MessageCircle, Users, type LucideIcon } from "lucide-react";

const ABOUT_LINKS = {
    github: "https://github.com/rons-space/Wanderer",
    telegramChannel: "", // Set your public channel URL (e.g. https://t.me/your_channel)
    supportGroup: "", // Set your support group URL (e.g. https://t.me/your_group)
    donate: "", // Set your donation URL (e.g. https://buymeacoffee.com/yourname)
};

/**
 * One row of the About tab's link list. The four rows were copies of each
 * other, and the copy/open buttons in each carried no accessible name: they
 * render an icon only, so a screen reader announced four pairs of unlabelled
 * buttons. The title now names the buttons as well as the row.
 */
function AboutLink({
    icon: Icon,
    title,
    url,
    onCopy,
    onOpen,
}: {
    icon: LucideIcon;
    title: string;
    url: string;
    onCopy: () => void;
    onOpen: () => void;
}) {
    return (
        <div className="rounded-lg border p-3 space-y-3">
            <div className="flex items-center justify-between gap-3">
                <div className="flex items-center gap-2 min-w-0">
                    <Icon className="h-4 w-4 text-muted-foreground" aria-hidden="true" />
                    <div className="min-w-0">
                        <p className="text-sm font-medium">{title}</p>
                        <p className="text-muted-foreground truncate text-xs">{url || "Not configured yet"}</p>
                    </div>
                </div>
                <div className="flex items-center gap-2">
                    <Button size="icon" variant="outline" disabled={!url} onClick={onCopy} aria-label={`Copy ${title} link`}>
                        <Copy className="h-4 w-4" aria-hidden="true" />
                    </Button>
                    <Button size="icon" variant="outline" disabled={!url} onClick={onOpen} aria-label={`Open ${title} link`}>
                        <ExternalLink className="h-4 w-4" aria-hidden="true" />
                    </Button>
                </div>
            </div>
        </div>
    );
}

export function AboutTab() {
    const [appVersion, setAppVersion] = useState<string>("Loading...");
    const [logPath, setLogPath] = useState<string | null>(null);

    useEffect(() => {
        const load = async () => {
            try {
                setAppVersion(await getVersion());
            } catch (e) {
                console.error("Failed to load app version:", e);
                setAppVersion("Unknown");
            }
            try {
                setLogPath(await api.getLogPath());
            } catch (e) {
                console.error("Failed to load the log path:", e);
            }
        };
        load();
    }, []);

    const openExternalLink = async (url: string) => {
        if (!url) {
            toast.info("Link not configured yet");
            return;
        }
        try {
            await openUrl(url);
        } catch (e) {
            console.error("Failed to open link:", e);
            toast.error("Failed to open link");
        }
    };

    const copyText = async (value: string, successMessage: string) => {
        if (!value) {
            toast.info("Link not configured yet");
            return;
        }
        try {
            await navigator.clipboard.writeText(value);
            toast.success(successMessage);
        } catch (e) {
            console.error("Failed to copy to clipboard:", e);
            toast.error("Failed to copy");
        }
    };

    return (
        <TabsContent value="about" className="mt-6 space-y-4">
            <Card>
                <CardHeader>
                    <CardTitle>About</CardTitle>
                    <CardDescription>Project information and support links</CardDescription>
                </CardHeader>
                <CardContent className="space-y-6">
                    <div className="flex items-center justify-between">
                        <Label>App Version</Label>
                        <span className="text-sm font-mono bg-muted px-2 py-1 rounded">
                            {appVersion}
                        </span>
                    </div>

                    {/*
                        A packaged build has no console, so this file
                        is the only record of a crash. Surfacing the
                        path is what makes a bug report possible.
                    */}
                    <div className="flex items-center justify-between gap-3">
                        <div className="min-w-0">
                            <Label>Log File</Label>
                            <p className="text-muted-foreground truncate text-xs">
                                {logPath || "Not available"}
                            </p>
                        </div>
                        <Button
                            size="icon"
                            variant="outline"
                            disabled={!logPath}
                            aria-label="Copy log file path"
                            onClick={() => copyText(logPath ?? "", "Log file path copied")}
                        >
                            <Copy className="h-4 w-4" aria-hidden="true" />
                        </Button>
                    </div>

                    <Separator />

                    <div className="space-y-3">
                        <Label>Links</Label>

                        <AboutLink
                            icon={Github}
                            title="GitHub Repository"
                            url={ABOUT_LINKS.github}
                            onCopy={() => copyText(ABOUT_LINKS.github, "GitHub link copied")}
                            onOpen={() => openExternalLink(ABOUT_LINKS.github)}
                        />
                        <AboutLink
                            icon={MessageCircle}
                            title="Telegram Channel"
                            url={ABOUT_LINKS.telegramChannel}
                            onCopy={() => copyText(ABOUT_LINKS.telegramChannel, "Telegram channel link copied")}
                            onOpen={() => openExternalLink(ABOUT_LINKS.telegramChannel)}
                        />
                        <AboutLink
                            icon={Users}
                            title="Support Group"
                            url={ABOUT_LINKS.supportGroup}
                            onCopy={() => copyText(ABOUT_LINKS.supportGroup, "Support group link copied")}
                            onOpen={() => openExternalLink(ABOUT_LINKS.supportGroup)}
                        />
                        <AboutLink
                            icon={HandHeart}
                            title="Donation"
                            url={ABOUT_LINKS.donate}
                            onCopy={() => copyText(ABOUT_LINKS.donate, "Donation link copied")}
                            onOpen={() => openExternalLink(ABOUT_LINKS.donate)}
                        />
                    </div>
                </CardContent>
            </Card>
        </TabsContent>
    );
}

