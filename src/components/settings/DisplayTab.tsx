import {
    appearanceModeConfig,
    cornerStyleConfig,
    iconStyleConfig,
    themeConfig,
    useTheme,
    type ThemeAppearanceMode,
    type ThemeCornerStyle,
    type ThemeIconStyle,
    type ThemeVariant,
} from "@/contexts/ThemeContext";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../ui/card";
import { Label } from "../ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "../ui/select";
import { Separator } from "../ui/separator";
import { Switch } from "../ui/switch";
import { TabsContent } from "../ui/tabs";
import type { AppConfig } from "./use-app-config";

interface DisplayTabProps {
    config: AppConfig;
    isSaving: boolean;
    saveConfig: (key: keyof AppConfig, value: string) => Promise<void>;
}

export function DisplayTab({ config, isSaving, saveConfig }: DisplayTabProps) {
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

    const timelineGrouping = config.timeline_grouping || "day";
    const themeVariants = Object.keys(themeConfig) as ThemeVariant[];
    const cornerVariants = Object.keys(cornerStyleConfig) as ThemeCornerStyle[];
    const iconVariants = Object.keys(iconStyleConfig) as ThemeIconStyle[];
    const appearanceVariants = Object.keys(appearanceModeConfig) as ThemeAppearanceMode[];
    // Only the two mobile-styled themes have a light/dark switch of their own.
    const supportsAppearanceMode = theme === "ios26" || theme === "android16";

    return (
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
    );
}
