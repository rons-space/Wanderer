import { useState, useEffect } from "react";
import { api, errorMessage, MigrationStatus } from "../lib/api";
import { Card, CardHeader, CardTitle, CardDescription, CardContent, CardFooter } from "./ui/card";
import { Label } from "./ui/label";
import { Input } from "./ui/input";
import { Button } from "./ui/button";
import { Loader2, HardDrive, Brain, User, LayoutGrid, Copy, Info, Github, MessageCircle, Users, HandHeart, ExternalLink } from "lucide-react";
import { Alert, AlertTitle, AlertDescription } from "./ui/alert";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "./ui/tabs";
import { Switch } from "./ui/switch";
import { Slider } from "./ui/slider";
import { Separator } from "./ui/separator";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./ui/select";
import { toast } from "sonner";
import { subscribe } from "@/lib/events";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Progress } from "./ui/progress";
import {
    useTheme,
    themeConfig,
    cornerStyleConfig,
    iconStyleConfig,
    appearanceModeConfig,
    type ThemeVariant,
    type ThemeCornerStyle,
    type ThemeIconStyle,
    type ThemeAppearanceMode
} from "@/contexts/ThemeContext";

interface AppConfig {
    cache_size_mb: string;
    view_cache_max_size_mb: string;
    view_cache_retention_hours: string;
    ai_face_enabled: string;
    ai_tags_enabled: string;
    timeline_grouping: string; // 'day' | 'month' | 'year'
}

const DEFAULT_CONFIG: AppConfig = {
    cache_size_mb: "5000",
    view_cache_max_size_mb: "2000",
    view_cache_retention_hours: "24",
    ai_face_enabled: "false",
    ai_tags_enabled: "false",
    timeline_grouping: "day",
};

type ModelDownloadProgress = {
    model: string;
    current: number;
    total: number;
};

const ABOUT_LINKS = {
    github: "https://github.com/rons-space/Wanderer",
    telegramChannel: "", // Set your public channel URL (e.g. https://t.me/your_channel)
    supportGroup: "", // Set your support group URL (e.g. https://t.me/your_group)
    donate: "", // Set your donation URL (e.g. https://buymeacoffee.com/yourname)
};

export function Settings() {
    const [user, setUser] = useState<string | null>(null);
    const [phone, setPhone] = useState("");
    const [code, setCode] = useState("");
    const [isLoading, setIsLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [step, setStep] = useState<'phone' | 'code' | 'authenticated'>('phone');
    const [config, setConfig] = useState<AppConfig>(DEFAULT_CONFIG);
    const [isSaving, setIsSaving] = useState(false);
    const [backupPath, setBackupPath] = useState<string>("");
    const [appVersion, setAppVersion] = useState<string>("Loading...");
    const [securityStatus, setSecurityStatus] = useState<{
        onboardingComplete: boolean;
        securityMode: string;
        encryptionConfigured: boolean;
        encryptionLocked: boolean;
        telegramCredentialsConfigured: boolean;
        migration: MigrationStatus;
    } | null>(null);
    const [securityPassphrase, setSecurityPassphrase] = useState("");
    const [securityPassphraseConfirm, setSecurityPassphraseConfirm] = useState("");
    const [generatedRecoveryKey, setGeneratedRecoveryKey] = useState<string | null>(null);
    const [migrationStatus, setMigrationStatus] = useState<MigrationStatus | null>(null);
    const [isRetryingPurge, setIsRetryingPurge] = useState(false);

    // CLIP State
    const [clipInstalled, setClipInstalled] = useState(false);
    const [isDownloadingModels, setIsDownloadingModels] = useState(false);
    const [downloadProgress, setDownloadProgress] = useState<ModelDownloadProgress | null>(null);
    const {
        theme,
        setTheme,
        animationsEnabled,
        setAnimationsEnabled,
        glassEffectsEnabled,
        setGlassEffectsEnabled,
        cornerStyle,
        setCornerStyle,
        iconStyle,
        setIconStyle,
        appearanceMode,
        setAppearanceMode,
    } = useTheme();

    useEffect(() => {
        const handleAuthChange = () => checkAuth();

        checkAuth();
        loadConfig();
        loadBackupPath();
        loadAppVersion();
        checkClipStatus();
        loadSecurityStatus();

        window.addEventListener('auth-changed', handleAuthChange);

        const unsubscribe = subscribe<ModelDownloadProgress>(
            "model_download_progress",
            (event) => {
                setDownloadProgress(event.payload);
            },
        );

        return () => {
            window.removeEventListener('auth-changed', handleAuthChange);
            unsubscribe();
        }
    }, []);

    const handleLogout = async () => {
        if (!confirm("Are you sure you want to disconnect? This will remove your local session file.")) return;
        setIsLoading(true);
        try {
            await api.logout();
            setUser(null);
            setStep('phone');
            toast.success("Disconnected successfully");
            window.dispatchEvent(new Event('auth-changed'));
        } catch (e) {
            console.error(e);
            toast.error("Failed to disconnect");
        } finally {
            setIsLoading(false);
        }
    };

    const checkClipStatus = async () => {
        try {
            const installed = await api.checkClipModels();
            setClipInstalled(installed);
        } catch (e) {
            console.error("Failed to check CLIP status:", e);
        }
    };

    const loadConfig = async () => {
        try {
            const data = await api.getAllConfig();
            setConfig({
                cache_size_mb: data.cache_size_mb || DEFAULT_CONFIG.cache_size_mb,
                view_cache_max_size_mb: data.view_cache_max_size_mb || DEFAULT_CONFIG.view_cache_max_size_mb,
                view_cache_retention_hours: data.view_cache_retention_hours || DEFAULT_CONFIG.view_cache_retention_hours,
                ai_face_enabled: data.ai_face_enabled || DEFAULT_CONFIG.ai_face_enabled,
                ai_tags_enabled: data.ai_tags_enabled || DEFAULT_CONFIG.ai_tags_enabled,
                timeline_grouping: data.timeline_grouping || DEFAULT_CONFIG.timeline_grouping,
            });
        } catch (e) {
            console.error("Failed to load config:", e);
        }
    };

    const loadBackupPath = async () => {
        try {
            const path = await api.getBackupPath();
            setBackupPath(path);
        } catch (e) {
            console.error("Failed to load backup path:", e);
            setBackupPath("");
        }
    };

    const loadAppVersion = async () => {
        try {
            const version = await getVersion();
            setAppVersion(version);
        } catch (e) {
            console.error("Failed to load app version:", e);
            setAppVersion("Unknown");
        }
    };

    const loadSecurityStatus = async () => {
        try {
            const status = await api.getSecurityStatus();
            setSecurityStatus(status);
            setMigrationStatus(status.migration);
        } catch (e) {
            console.error("Failed to load security status:", e);
            setSecurityStatus(null);
            setMigrationStatus(null);
        }
    };

    const loadMigrationStatus = async () => {
        try {
            const status = await api.getEncryptionMigrationStatus();
            setMigrationStatus(status);
        } catch (e) {
            console.error("Failed to load migration status:", e);
        }
    };

    useEffect(() => {
        if (securityStatus?.securityMode !== "encrypted") return;
        loadMigrationStatus();
        const id = setInterval(() => {
            loadMigrationStatus();
        }, 3000);
        return () => clearInterval(id);
    }, [securityStatus?.securityMode]);

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

    const saveConfig = async (key: keyof AppConfig, value: string) => {
        setIsSaving(true);
        try {
            await api.setConfig(key, value);
            setConfig(prev => ({ ...prev, [key]: value }));
            toast.success("Settings saved");
        } catch (e) {
            console.error("Failed to save setting:", e);
            toast.error("Failed to save setting");
        } finally {
            setIsSaving(false);
        }
    };

    const enableEncryption = async () => {
        if (securityPassphrase.length < 8) {
            toast.error("Passphrase must be at least 8 characters");
            return;
        }
        if (securityPassphrase !== securityPassphraseConfirm) {
            toast.error("Passphrase confirmation does not match");
            return;
        }

        setIsSaving(true);
        try {
            const result = await api.initializeEncryption(securityPassphrase);
            setGeneratedRecoveryKey(result.recoveryKey);
            toast.success("Encryption enabled. Save your recovery key now.");

            try {
                await api.startEncryptionMigration();
                toast.info("Started background migration of existing uploaded media.");
                await loadMigrationStatus();
            } catch (e) {
                console.warn("Migration start failed:", e);
            }

            setSecurityPassphrase("");
            setSecurityPassphraseConfirm("");
            await loadSecurityStatus();
        } catch (e) {
            toast.error(`Failed to enable encryption: ${errorMessage(e)}`);
        } finally {
            setIsSaving(false);
        }
    };

    const checkAuth = async () => {
        try {
            const me = await api.getMe();
            if (me) {
                setUser(me);
                setStep('authenticated');
            } else {
                setUser(null);
                setStep('phone');
            }
        } catch (e) {
            console.error("Failed to load the Telegram session:", e);
            setUser(null);
            setStep('phone');
        }
    };

    const handleRequestCode = async (e: React.FormEvent) => {
        e.preventDefault();
        setIsLoading(true);
        setError(null);
        try {
            await api.loginRequestCode(phone);
            setStep('code');
        } catch (err) {
            console.error(err);
            setError(String(err) || "Failed to send code");
        } finally {
            setIsLoading(false);
        }
    };

    const handleSignIn = async (e: React.FormEvent) => {
        e.preventDefault();
        setIsLoading(true);
        setError(null);
        try {
            const loggedInUser = await api.loginSignIn(code);
            setUser(loggedInUser);
            setStep('authenticated');
            window.dispatchEvent(new Event('auth-changed'));
        } catch (err) {
            console.error(err);
            setError(String(err) || "Failed to sign in");
        } finally {
            setIsLoading(false);
        }
    };

    const cacheSizeMb = parseInt(config.cache_size_mb) || 5000;
    const viewCacheSizeMb = parseInt(config.view_cache_max_size_mb) || 2000;
    const viewCacheRetentionHours = parseInt(config.view_cache_retention_hours) || 24;
    const aiFaceEnabled = config.ai_face_enabled === "true";
    const aiTagsEnabled = config.ai_tags_enabled === "true";
    const timelineGrouping = config.timeline_grouping || "day";
    const themeVariants = Object.keys(themeConfig) as ThemeVariant[];
    const cornerVariants = Object.keys(cornerStyleConfig) as ThemeCornerStyle[];
    const iconVariants = Object.keys(iconStyleConfig) as ThemeIconStyle[];
    const appearanceVariants = Object.keys(appearanceModeConfig) as ThemeAppearanceMode[];
    const supportsAppearanceMode = theme === "ios26" || theme === "android16";

    return (
        <div className="h-full overflow-auto p-6">
            <div className="max-w-2xl mx-auto space-y-6">
                <div>
                    <h1 className="text-2xl font-bold">Settings</h1>
                    <p className="text-muted-foreground">Configure your Wander(er) preferences</p>
                </div>

                <Tabs defaultValue="account" className="w-full">
                    <TabsList className="grid w-full grid-cols-5">
                        <TabsTrigger value="account" className="flex items-center gap-2">
                            <User className="h-4 w-4" />
                            Account
                        </TabsTrigger>
                        <TabsTrigger value="display" className="flex items-center gap-2">
                            <LayoutGrid className="h-4 w-4" />
                            Display
                        </TabsTrigger>
                        <TabsTrigger value="storage" className="flex items-center gap-2">
                            <HardDrive className="h-4 w-4" />
                            Storage
                        </TabsTrigger>
                        <TabsTrigger value="ai" className="flex items-center gap-2">
                            <Brain className="h-4 w-4" />
                            AI
                        </TabsTrigger>
                        <TabsTrigger value="about" className="flex items-center gap-2">
                            <Info className="h-4 w-4" />
                            About
                        </TabsTrigger>
                    </TabsList>

                    {/* Account Tab */}
                    <TabsContent value="account" className="mt-6 space-y-4">
                        <Card>
                            <CardHeader>
                                <CardTitle>Telegram Account</CardTitle>
                                <CardDescription>
                                    {step === 'authenticated'
                                        ? "Your account is connected to Telegram"
                                        : "Connect your Telegram account to backup photos"}
                                </CardDescription>
                            </CardHeader>
                            <CardContent>
                                {step === 'authenticated' ? (
                                    <div className="space-y-4">
                                        <div className="space-y-2">
                                            <Label>Logged in as</Label>
                                            <div className="bg-muted p-3 rounded-md font-mono">{user}</div>
                                        </div>
                                        <Alert>
                                            <AlertTitle>✓ Telegram Connected</AlertTitle>
                                            <AlertDescription>Your photos are being backed up to Saved Messages.</AlertDescription>
                                        </Alert>
                                        <Button
                                            variant="destructive"
                                            className="w-full mt-4"
                                            onClick={handleLogout}
                                            disabled={isLoading}
                                        >
                                            Disconnect Account
                                        </Button>
                                    </div>
                                ) : step === 'phone' ? (
                                    <form onSubmit={handleRequestCode} className="space-y-4">
                                        {error && (
                                            <div className="bg-red-50 text-red-500 p-3 rounded text-sm">{error}</div>
                                        )}
                                        <div className="space-y-2">
                                            <Label htmlFor="phone">Phone Number</Label>
                                            <Input
                                                id="phone"
                                                placeholder="+1234567890"
                                                value={phone}
                                                onChange={(e) => setPhone(e.target.value)}
                                                required
                                            />
                                            <p className="text-xs text-muted-foreground">Include country code</p>
                                        </div>
                                        <Button type="submit" className="w-full" disabled={isLoading}>
                                            {isLoading && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                                            Send Code
                                        </Button>
                                    </form>
                                ) : (
                                    <form onSubmit={handleSignIn} className="space-y-4">
                                        {error && (
                                            <div className="bg-red-50 text-red-500 p-3 rounded text-sm">{error}</div>
                                        )}
                                        <div className="space-y-2">
                                            <Label htmlFor="code">Verification Code</Label>
                                            <Input
                                                id="code"
                                                placeholder="123456"
                                                value={code}
                                                onChange={(e) => setCode(e.target.value)}
                                                required
                                            />
                                        </div>
                                        <Button type="submit" className="w-full" disabled={isLoading}>
                                            {isLoading && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                                            Sign In
                                        </Button>
                                        <Button variant="link" onClick={() => setStep('phone')} type="button" className="w-full">
                                            Back to Phone Number
                                        </Button>
                                    </form>
                                )}
                            </CardContent>
                            {step === 'authenticated' && (
                                <CardFooter>
                                    <Button variant="outline" className="w-full" disabled>
                                        Log Out (Not Implemented)
                                    </Button>
                                </CardFooter>
                            )}
                        </Card>

                        <Card>
                            <CardHeader>
                                <CardTitle>Security</CardTitle>
                                <CardDescription>
                                    Encryption mode and recovery settings for this device.
                                </CardDescription>
                            </CardHeader>
                            <CardContent className="space-y-4">
                                <div className="flex items-center justify-between">
                                    <Label>Current Mode</Label>
                                    <span className="text-sm font-medium">
                                        {securityStatus?.securityMode === "encrypted"
                                            ? "Encrypted"
                                            : securityStatus?.securityMode === "unencrypted"
                                                ? "Unencrypted"
                                                : "Not configured"}
                                    </span>
                                </div>

                                {securityStatus?.securityMode === "encrypted" && (
                                    <Alert>
                                        <AlertTitle>Encryption Enabled</AlertTitle>
                                        <AlertDescription>
                                            This mode is one-way. To avoid privacy regressions, disabling encryption is not available.
                                        </AlertDescription>
                                    </Alert>
                                )}

                                {securityStatus?.securityMode === "encrypted" && (
                                    <div className="space-y-3 rounded-md border p-3">
                                        <div className="flex items-center justify-between">
                                            <Label>Migration Progress</Label>
                                            <span className="text-xs text-muted-foreground">
                                                {(migrationStatus?.running ?? false) ? "Running" : "Idle"}
                                            </span>
                                        </div>
                                        <Progress
                                            value={
                                                (migrationStatus?.total ?? 0) > 0
                                                    ? Math.min(
                                                        100,
                                                        ((migrationStatus?.processed ?? 0) / (migrationStatus?.total ?? 1)) * 100
                                                    )
                                                    : 0
                                            }
                                        />
                                        <div className="grid grid-cols-2 gap-2 text-xs text-muted-foreground">
                                            <p>Total: {migrationStatus?.total ?? 0}</p>
                                            <p>Processed: {migrationStatus?.processed ?? 0}</p>
                                            <p>Succeeded: {migrationStatus?.succeeded ?? 0}</p>
                                            <p>Failed: {migrationStatus?.failed ?? 0}</p>
                                        </div>
                                        {(migrationStatus?.unpurgedPlaintext?.length ?? 0) > 0 && (
                                            <Alert variant="destructive">
                                                <AlertTitle>Unencrypted copies still in Telegram</AlertTitle>
                                                <AlertDescription className="space-y-2 text-xs">
                                                    <p>
                                                        {migrationStatus?.unpurgedPlaintext.length} item(s) were
                                                        re-uploaded encrypted, but their original unencrypted copy
                                                        could not be confirmed deleted from Telegram.
                                                    </p>
                                                    <Button
                                                        variant="outline"
                                                        size="sm"
                                                        disabled={isRetryingPurge}
                                                        onClick={async () => {
                                                            setIsRetryingPurge(true);
                                                            try {
                                                                const purged = await api.retryPlaintextPurge();
                                                                toast.success(
                                                                    `Deleted ${purged} unencrypted copies`
                                                                );
                                                            } catch (e) {
                                                                toast.error(`Purge retry failed: ${errorMessage(e)}`);
                                                            } finally {
                                                                setIsRetryingPurge(false);
                                                                await loadMigrationStatus();
                                                            }
                                                        }}
                                                    >
                                                        {isRetryingPurge ? "Deleting..." : "Retry deletion"}
                                                    </Button>
                                                </AlertDescription>
                                            </Alert>
                                        )}
                                        {migrationStatus?.lastError && (
                                            <Alert>
                                                <AlertTitle>Last Migration Error</AlertTitle>
                                                <AlertDescription className="text-xs break-all">
                                                    {migrationStatus.lastError}
                                                </AlertDescription>
                                            </Alert>
                                        )}
                                        <div className="flex gap-2">
                                            <Button
                                                variant="outline"
                                                className="w-full"
                                                disabled={migrationStatus?.running}
                                                onClick={async () => {
                                                    try {
                                                        await api.startEncryptionMigration();
                                                        toast.success("Migration started");
                                                        await loadMigrationStatus();
                                                    } catch (e) {
                                                        toast.error(`Failed to start migration: ${errorMessage(e)}`);
                                                    }
                                                }}
                                            >
                                                Resume Migration
                                            </Button>
                                            <Button
                                                variant="outline"
                                                className="w-full"
                                                onClick={loadMigrationStatus}
                                            >
                                                Refresh Status
                                            </Button>
                                        </div>
                                    </div>
                                )}

                                {securityStatus?.securityMode !== "encrypted" && (
                                    <div className="space-y-3 rounded-md border p-3">
                                        <Alert>
                                            <AlertTitle>Enable Encryption (One-way)</AlertTitle>
                                            <AlertDescription>
                                                Once enabled, you cannot switch back to unencrypted mode without a full reset.
                                            </AlertDescription>
                                        </Alert>
                                        <div className="space-y-2">
                                            <Label htmlFor="security-passphrase">New Passphrase</Label>
                                            <Input
                                                id="security-passphrase"
                                                type="password"
                                                value={securityPassphrase}
                                                onChange={(e) => setSecurityPassphrase(e.target.value)}
                                                placeholder="At least 8 characters"
                                            />
                                        </div>
                                        <div className="space-y-2">
                                            <Label htmlFor="security-passphrase-confirm">Confirm Passphrase</Label>
                                            <Input
                                                id="security-passphrase-confirm"
                                                type="password"
                                                value={securityPassphraseConfirm}
                                                onChange={(e) => setSecurityPassphraseConfirm(e.target.value)}
                                                placeholder="Repeat passphrase"
                                            />
                                        </div>
                                        <Button onClick={enableEncryption} disabled={isSaving} className="w-full">
                                            {isSaving && <Loader2 className="mr-2 h-4 w-4 animate-spin" />}
                                            Enable Encryption
                                        </Button>
                                    </div>
                                )}

                                {generatedRecoveryKey && (
                                    <div className="space-y-2 rounded-md border bg-muted p-3">
                                        <Label>Recovery Key (shown once)</Label>
                                        <p className="font-mono text-xs break-all">{generatedRecoveryKey}</p>
                                        <p className="text-xs text-muted-foreground">
                                            Save this key securely. It is required if passphrase is lost.
                                        </p>
                                    </div>
                                )}
                            </CardContent>
                        </Card>

                    </TabsContent>

                    {/* Display Tab */}
                    <TabsContent value="display" className="mt-6">
                        <Card>
                            <CardHeader>
                                <CardTitle>Display Settings</CardTitle>
                                <CardDescription>Customize how your photos are displayed</CardDescription>
                            </CardHeader>
                            <CardContent className="space-y-6">
                                <div className="space-y-3">
                                    <Label htmlFor="theme-preset">Theme Preset</Label>
                                    <Select
                                        value={theme}
                                        onValueChange={(value) => setTheme(value as ThemeVariant)}
                                    >
                                        <SelectTrigger id="theme-preset" className="w-full">
                                            <SelectValue placeholder="Select theme" />
                                        </SelectTrigger>
                                        <SelectContent>
                                            {themeVariants.map((variant) => (
                                                <SelectItem key={variant} value={variant}>
                                                    {themeConfig[variant].name}
                                                </SelectItem>
                                            ))}
                                        </SelectContent>
                                    </Select>
                                    <p className="text-xs text-muted-foreground">
                                        Choose the overall visual style used by the app interface.
                                    </p>
                                </div>

                                <Separator />

                                <div className="space-y-4">
                                    <div className="space-y-3">
                                        <Label htmlFor="appearance-mode">Appearance Mode</Label>
                                        <Select
                                            value={appearanceMode}
                                            onValueChange={(value) => setAppearanceMode(value as ThemeAppearanceMode)}
                                            disabled={!supportsAppearanceMode}
                                        >
                                            <SelectTrigger id="appearance-mode" className="w-full">
                                                <SelectValue placeholder="Select appearance mode" />
                                            </SelectTrigger>
                                            <SelectContent>
                                                {appearanceVariants.map((variant) => (
                                                    <SelectItem key={variant} value={variant}>
                                                        {appearanceModeConfig[variant].name}
                                                    </SelectItem>
                                                ))}
                                            </SelectContent>
                                        </Select>
                                        <p className="text-xs text-muted-foreground">
                                            {supportsAppearanceMode
                                                ? appearanceModeConfig[appearanceMode].description
                                                : "Available for iOS 26 and Android 16 themes. Default is Dark."}
                                        </p>
                                    </div>

                                    <Separator />

                                    <div className="flex items-center justify-between">
                                        <div className="space-y-1">
                                            <Label htmlFor="theme-animations">Theme Animations</Label>
                                            <p className="text-xs text-muted-foreground">
                                                Enable transitions, fade-ins, and theme motion effects.
                                            </p>
                                        </div>
                                        <Switch
                                            id="theme-animations"
                                            checked={animationsEnabled}
                                            onCheckedChange={setAnimationsEnabled}
                                        />
                                    </div>

                                    <div className="flex items-center justify-between">
                                        <div className="space-y-1">
                                            <Label htmlFor="theme-glass">Glass Effects</Label>
                                            <p className="text-xs text-muted-foreground">
                                                Use blur/translucent surfaces for compatible themes.
                                            </p>
                                        </div>
                                        <Switch
                                            id="theme-glass"
                                            checked={glassEffectsEnabled}
                                            onCheckedChange={setGlassEffectsEnabled}
                                        />
                                    </div>
                                </div>

                                <Separator />

                                <div className="space-y-3">
                                    <Label htmlFor="corner-style">Corner Style</Label>
                                    <Select
                                        value={cornerStyle}
                                        onValueChange={(value) => setCornerStyle(value as ThemeCornerStyle)}
                                    >
                                        <SelectTrigger id="corner-style" className="w-full">
                                            <SelectValue placeholder="Select corner style" />
                                        </SelectTrigger>
                                        <SelectContent>
                                            {cornerVariants.map((variant) => (
                                                <SelectItem key={variant} value={variant}>
                                                    {cornerStyleConfig[variant].name}
                                                </SelectItem>
                                            ))}
                                        </SelectContent>
                                    </Select>
                                    <p className="text-xs text-muted-foreground">
                                        {cornerStyleConfig[cornerStyle].description}
                                    </p>
                                </div>

                                <div className="space-y-3">
                                    <Label htmlFor="icon-style">Icon Style</Label>
                                    <Select
                                        value={iconStyle}
                                        onValueChange={(value) => setIconStyle(value as ThemeIconStyle)}
                                    >
                                        <SelectTrigger id="icon-style" className="w-full">
                                            <SelectValue placeholder="Select icon style" />
                                        </SelectTrigger>
                                        <SelectContent>
                                            {iconVariants.map((variant) => (
                                                <SelectItem key={variant} value={variant}>
                                                    {iconStyleConfig[variant].name}
                                                </SelectItem>
                                            ))}
                                        </SelectContent>
                                    </Select>
                                    <p className="text-xs text-muted-foreground">
                                        {iconStyleConfig[iconStyle].description}
                                    </p>
                                </div>

                                <Separator />

                                <div className="space-y-3">
                                    <Label htmlFor="timeline-grouping">Timeline Grouping</Label>
                                    <Select
                                        value={timelineGrouping}
                                        onValueChange={(value) => saveConfig("timeline_grouping", value)}
                                        disabled={isSaving}
                                    >
                                        <SelectTrigger id="timeline-grouping" className="w-full">
                                            <SelectValue placeholder="Select grouping" />
                                        </SelectTrigger>
                                        <SelectContent>
                                            <SelectItem value="day">Day (January 21, 2026)</SelectItem>
                                            <SelectItem value="month">Month (January 2026)</SelectItem>
                                            <SelectItem value="year">Year (2026)</SelectItem>
                                        </SelectContent>
                                    </Select>
                                    <p className="text-xs text-muted-foreground">
                                        Choose how photos are grouped in the timeline view. Date headers will appear between groups.
                                    </p>
                                </div>
                            </CardContent>
                        </Card>
                    </TabsContent>


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
                                        <Label>Maximum Cache Size</Label>
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
                                            setConfig(prev => ({ ...prev, cache_size_mb: String(next) }));
                                        }}
                                        onValueCommit={(value) => {
                                            const next = value[0];
                                            if (typeof next !== "number") return;
                                            saveConfig("cache_size_mb", String(next));
                                        }}
                                        disabled={isSaving}
                                    />
                                    <p className="text-xs text-muted-foreground">
                                        Thumbnails and recently viewed photos are cached locally for fast access.
                                        Older cached items are automatically removed when the limit is reached.
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
                                            setConfig(prev => ({ ...prev, view_cache_max_size_mb: String(next) }));
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
                                            setConfig(prev => ({ ...prev, view_cache_retention_hours: String(next) }));
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

                    {/* AI Tab */}
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

                    {/* About Tab */}
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

                                <Separator />

                                <div className="space-y-3">
                                    <Label>Links</Label>

                                    <div className="rounded-lg border p-3 space-y-3">
                                        <div className="flex items-center justify-between gap-3">
                                            <div className="flex items-center gap-2 min-w-0">
                                                <Github className="h-4 w-4 text-muted-foreground" />
                                                <div className="min-w-0">
                                                    <p className="text-sm font-medium">GitHub Repository</p>
                                                    <p className="text-xs text-muted-foreground truncate">{ABOUT_LINKS.github}</p>
                                                </div>
                                            </div>
                                            <div className="flex items-center gap-2">
                                                <Button size="icon" variant="outline" onClick={() => copyText(ABOUT_LINKS.github, "GitHub link copied")}>
                                                    <Copy className="h-4 w-4" />
                                                </Button>
                                                <Button size="icon" variant="outline" onClick={() => openExternalLink(ABOUT_LINKS.github)}>
                                                    <ExternalLink className="h-4 w-4" />
                                                </Button>
                                            </div>
                                        </div>
                                    </div>

                                    <div className="rounded-lg border p-3 space-y-3">
                                        <div className="flex items-center justify-between gap-3">
                                            <div className="flex items-center gap-2 min-w-0">
                                                <MessageCircle className="h-4 w-4 text-muted-foreground" />
                                                <div className="min-w-0">
                                                    <p className="text-sm font-medium">Telegram Channel</p>
                                                    <p className="text-xs text-muted-foreground truncate">
                                                        {ABOUT_LINKS.telegramChannel || "Not configured yet"}
                                                    </p>
                                                </div>
                                            </div>
                                            <div className="flex items-center gap-2">
                                                <Button
                                                    size="icon"
                                                    variant="outline"
                                                    disabled={!ABOUT_LINKS.telegramChannel}
                                                    onClick={() => copyText(ABOUT_LINKS.telegramChannel, "Telegram channel link copied")}
                                                >
                                                    <Copy className="h-4 w-4" />
                                                </Button>
                                                <Button
                                                    size="icon"
                                                    variant="outline"
                                                    disabled={!ABOUT_LINKS.telegramChannel}
                                                    onClick={() => openExternalLink(ABOUT_LINKS.telegramChannel)}
                                                >
                                                    <ExternalLink className="h-4 w-4" />
                                                </Button>
                                            </div>
                                        </div>
                                    </div>

                                    <div className="rounded-lg border p-3 space-y-3">
                                        <div className="flex items-center justify-between gap-3">
                                            <div className="flex items-center gap-2 min-w-0">
                                                <Users className="h-4 w-4 text-muted-foreground" />
                                                <div className="min-w-0">
                                                    <p className="text-sm font-medium">Support Group</p>
                                                    <p className="text-xs text-muted-foreground truncate">
                                                        {ABOUT_LINKS.supportGroup || "Not configured yet"}
                                                    </p>
                                                </div>
                                            </div>
                                            <div className="flex items-center gap-2">
                                                <Button
                                                    size="icon"
                                                    variant="outline"
                                                    disabled={!ABOUT_LINKS.supportGroup}
                                                    onClick={() => copyText(ABOUT_LINKS.supportGroup, "Support group link copied")}
                                                >
                                                    <Copy className="h-4 w-4" />
                                                </Button>
                                                <Button
                                                    size="icon"
                                                    variant="outline"
                                                    disabled={!ABOUT_LINKS.supportGroup}
                                                    onClick={() => openExternalLink(ABOUT_LINKS.supportGroup)}
                                                >
                                                    <ExternalLink className="h-4 w-4" />
                                                </Button>
                                            </div>
                                        </div>
                                    </div>

                                    <div className="rounded-lg border p-3 space-y-3">
                                        <div className="flex items-center justify-between gap-3">
                                            <div className="flex items-center gap-2 min-w-0">
                                                <HandHeart className="h-4 w-4 text-muted-foreground" />
                                                <div className="min-w-0">
                                                    <p className="text-sm font-medium">Donation</p>
                                                    <p className="text-xs text-muted-foreground truncate">
                                                        {ABOUT_LINKS.donate || "Not configured yet"}
                                                    </p>
                                                </div>
                                            </div>
                                            <div className="flex items-center gap-2">
                                                <Button
                                                    size="icon"
                                                    variant="outline"
                                                    disabled={!ABOUT_LINKS.donate}
                                                    onClick={() => copyText(ABOUT_LINKS.donate, "Donation link copied")}
                                                >
                                                    <Copy className="h-4 w-4" />
                                                </Button>
                                                <Button
                                                    size="icon"
                                                    variant="outline"
                                                    disabled={!ABOUT_LINKS.donate}
                                                    onClick={() => openExternalLink(ABOUT_LINKS.donate)}
                                                >
                                                    <ExternalLink className="h-4 w-4" />
                                                </Button>
                                            </div>
                                        </div>
                                    </div>
                                </div>
                            </CardContent>
                        </Card>
                    </TabsContent>
                </Tabs>
            </div>
        </div>
    );
}
