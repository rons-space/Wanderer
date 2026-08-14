import { Tabs, TabsList, TabsTrigger } from "./ui/tabs";
import { Brain, HardDrive, Info, LayoutGrid, User } from "lucide-react";
import { AboutTab } from "./settings/AboutTab";
import { AccountTab } from "./settings/AccountTab";
import { AiTab } from "./settings/AiTab";
import { DisplayTab } from "./settings/DisplayTab";
import { StorageTab } from "./settings/StorageTab";
import { useAppConfig } from "./settings/use-app-config";
import { useSecurityStatus } from "./settings/use-security-status";

/**
 * The settings shell.
 *
 * Each tab owns the state only it uses and loads it on mount, so opening
 * Settings no longer fires every read the five tabs between them need. The two
 * pieces of state that genuinely cross tabs, the config table and the security
 * status, are held here and passed down.
 */
export function Settings() {
    const { config, isSaving, saveConfig, previewConfig } = useAppConfig();
    const { securityStatus, reload: reloadSecurityStatus } = useSecurityStatus();

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
                            <User className="w-4 h-4" aria-hidden="true" />
                            <span className="hidden sm:inline">Account</span>
                        </TabsTrigger>
                        <TabsTrigger value="display" className="flex items-center gap-2">
                            <LayoutGrid className="w-4 h-4" aria-hidden="true" />
                            <span className="hidden sm:inline">Display</span>
                        </TabsTrigger>
                        <TabsTrigger value="storage" className="flex items-center gap-2">
                            <HardDrive className="w-4 h-4" aria-hidden="true" />
                            <span className="hidden sm:inline">Storage</span>
                        </TabsTrigger>
                        <TabsTrigger value="ai" className="flex items-center gap-2">
                            <Brain className="w-4 h-4" aria-hidden="true" />
                            <span className="hidden sm:inline">AI</span>
                        </TabsTrigger>
                        <TabsTrigger value="about" className="flex items-center gap-2">
                            <Info className="w-4 h-4" aria-hidden="true" />
                            <span className="hidden sm:inline">About</span>
                        </TabsTrigger>
                    </TabsList>

                    <AccountTab
                        securityStatus={securityStatus}
                        reloadSecurityStatus={reloadSecurityStatus}
                    />
                    <DisplayTab config={config} isSaving={isSaving} saveConfig={saveConfig} />
                    <StorageTab
                        config={config}
                        isSaving={isSaving}
                        saveConfig={saveConfig}
                        previewConfig={previewConfig}
                        securityStatus={securityStatus}
                    />
                    <AiTab config={config} isSaving={isSaving} saveConfig={saveConfig} />
                    <AboutTab />
                </Tabs>
            </div>
        </div>
    );
}
