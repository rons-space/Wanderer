import { SidebarInset, SidebarProvider, SidebarTrigger } from "@/components/ui/sidebar";
import { AppSidebar } from "./AppSidebar";
import { Separator } from "@/components/ui/separator";
import {
    Breadcrumb,
    BreadcrumbItem,
    BreadcrumbList,
    BreadcrumbPage,
    BreadcrumbLink,
    BreadcrumbSeparator
} from "@/components/ui/breadcrumb";
import { useTheme } from "@/contexts/ThemeContext";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import {
    Search,
    LayoutGrid,
    List,
    RefreshCw,
    ChevronLeft,
    ChevronRight,
    Home
} from "lucide-react";
import { cn } from "@/lib/utils";
import { WindowControls, TitleBar } from "./WindowControls";

interface LayoutProps {
    children: React.ReactNode;
    currentView: string;
    onViewChange: (view: string) => void;
}

export function Layout({ children, currentView, onViewChange }: LayoutProps) {
    const { theme } = useTheme();
    const isIos26Theme = theme === 'ios26';
    const isAndroid16Theme = theme === 'android16';

    // View title mapping
    const viewTitles: Record<string, string> = {
        timeline: 'All Photos',
        albums: 'Albums',
        search: 'Search',
        settings: 'Settings',
        people: 'People',
        favorites: 'Favorites',
        trash: 'Trash',
        uploads: 'Uploads',
        map: 'Map',
        duplicates: 'Duplicates',
        tags: 'Tags',
        'smart-albums': 'Smart Albums',
        archive: 'Archive',
    };

    // ==========================================
    // EXPLORER THEME HEADER (Spacedrive-style)
    // ==========================================
    if (theme === 'explorer') {
        return (
            <SidebarProvider>
                <AppSidebar currentView={currentView} onViewChange={onViewChange} />
                <SidebarInset className="flex flex-col h-full overflow-hidden">
                    {/* Explorer Header with navigation, breadcrumb, search, and view toggles */}
                    <TitleBar className="flex h-12 shrink-0 items-center gap-2 border-b bg-sidebar px-3 transition-all">
                        {/* Window Controls (Traffic Lights) */}
                        <WindowControls className="mr-2" />

                        {/* Left: Sidebar trigger + Navigation arrows */}
                        <div className="flex items-center gap-1">
                            <SidebarTrigger className="h-8 w-8" />
                            <Separator orientation="vertical" className="h-5 mx-1" />
                            <Button variant="ghost" size="icon" className="h-8 w-8" aria-label="Back" disabled>
                                <ChevronLeft className="h-4 w-4" />
                            </Button>
                            <Button variant="ghost" size="icon" className="h-8 w-8" aria-label="Forward" disabled>
                                <ChevronRight className="h-4 w-4" />
                            </Button>
                        </div>

                        {/* Center: Breadcrumb */}
                        <div className="flex-1 flex items-center gap-2 px-2">
                            <Breadcrumb>
                                <BreadcrumbList>
                                    <BreadcrumbItem>
                                        <BreadcrumbLink
                                            onClick={() => onViewChange('timeline')}
                                            className="flex items-center gap-1.5 cursor-pointer hover:text-foreground transition-colors"
                                        >
                                            <Home className="h-3.5 w-3.5" />
                                        </BreadcrumbLink>
                                    </BreadcrumbItem>
                                    <BreadcrumbSeparator />
                                    <BreadcrumbItem>
                                        <BreadcrumbPage className="font-medium">
                                            {viewTitles[currentView] || currentView}
                                        </BreadcrumbPage>
                                    </BreadcrumbItem>
                                </BreadcrumbList>
                            </Breadcrumb>
                        </div>

                        {/* Right: Search + View toggles */}
                        <div className="flex items-center gap-2">
                            <div className="relative">
                                <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
                                <Input
                                    placeholder="Search"
                                    className="explorer-search h-8 w-48 pl-8 text-sm bg-input border-0"
                                    onClick={() => onViewChange('search')}
                                    readOnly
                                />
                            </div>
                            <Separator orientation="vertical" className="h-5" />
                            <div className="flex items-center gap-0.5">
                                <Button variant="ghost" size="icon" className="h-8 w-8" aria-label="Grid view">
                                    <LayoutGrid className="h-4 w-4" />
                                </Button>
                                <Button variant="ghost" size="icon" className="h-8 w-8" aria-label="List view">
                                    <List className="h-4 w-4" />
                                </Button>
                            </div>
                            <Button variant="ghost" size="icon" className="h-8 w-8" aria-label="Refresh">
                                <RefreshCw className="h-4 w-4" />
                            </Button>
                        </div>
                    </TitleBar>

                    <div className="flex-1 min-h-0 overflow-hidden">
                        {children}
                    </div>

                    {/* Explorer Footer - Path bar */}
                    <footer className="h-7 shrink-0 flex items-center px-4 border-t bg-sidebar text-xs text-muted-foreground">
                        <span>My Library</span>
                        <ChevronRight className="h-3 w-3 mx-1" />
                        <span>{viewTitles[currentView] || currentView}</span>
                    </footer>
                </SidebarInset>
            </SidebarProvider>
        );
    }

    // ==========================================
    // IMMERSIVE THEME HEADER (Minimal, photo-focused)
    // ==========================================
    return (
        <SidebarProvider>
            <AppSidebar currentView={currentView} onViewChange={onViewChange} />
            <SidebarInset className="flex flex-col h-full overflow-hidden">
                {/* Immersive Header - Minimal with floating style */}
                <TitleBar className={cn(
                    "flex h-14 shrink-0 items-center gap-3 px-4 transition-all",
                    "bg-background/80 backdrop-blur-md border-b border-border/50",
                    isIos26Theme && "ios26-titlebar-shell",
                    isAndroid16Theme && "android16-titlebar-shell"
                )}>
                    {/* Window Controls (Traffic Lights) */}
                    <WindowControls className="mr-1" />

                    <SidebarTrigger className="h-9 w-9 rounded-xl" />

                    {/* Title with subtle animation */}
                    <div className="flex-1">
                        <h1 className={cn(
                            "font-display text-lg tracking-tight animate-fade-in",
                            isAndroid16Theme ? "font-black tracking-[0.01em]" : "font-semibold"
                        )}>
                            {viewTitles[currentView] || currentView}
                        </h1>
                    </div>

                    {/* Search button - opens search view */}
                    <Button
                        variant="ghost"
                        size="sm"
                        onClick={() => onViewChange('search')}
                        className="gap-2 rounded-xl"
                    >
                        <Search className="h-4 w-4" />
                        <span className="hidden sm:inline">Search</span>
                    </Button>
                </TitleBar>

                <div className="flex-1 min-h-0 overflow-hidden page-content">
                    {children}
                </div>
            </SidebarInset>
        </SidebarProvider>
    );
}
