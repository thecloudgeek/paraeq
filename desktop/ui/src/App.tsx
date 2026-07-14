// The app shell: a five-tab layout in the prototype's exact order
// (Measure | Target | EQ | Analyzer | Profiles). Rust owns all state -- a single
// `useAppState()` here holds the latest snapshot and passes slices down; React
// keeps no forked copy. Only EQ has content this stage (a stub until Task 14);
// the rest name the stage they arrive in.

import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { useAppState } from "@/ipc/useAppState";
import { EqTab } from "@/tabs/EqTab";
import { PlaceholderTab } from "@/tabs/PlaceholderTab";

function App() {
  const state = useAppState();

  return (
    <div className="flex h-screen flex-col p-4">
      <Tabs defaultValue="eq" className="flex h-full flex-col">
        <TabsList>
          <TabsTrigger value="measure">Measure</TabsTrigger>
          <TabsTrigger value="target">Target</TabsTrigger>
          <TabsTrigger value="eq">EQ</TabsTrigger>
          <TabsTrigger value="analyzer">Analyzer</TabsTrigger>
          <TabsTrigger value="profiles">Profiles</TabsTrigger>
        </TabsList>

        <TabsContent value="measure">
          <PlaceholderTab stage="stage 6" title="Measure" />
        </TabsContent>
        <TabsContent value="target">
          <PlaceholderTab stage="stage 5" title="Target" />
        </TabsContent>
        <TabsContent value="eq" className="min-h-0 flex-1">
          {state ? (
            <EqTab state={state} />
          ) : (
            <div className="p-4 text-sm text-muted-foreground">loading app state…</div>
          )}
        </TabsContent>
        <TabsContent value="analyzer">
          <PlaceholderTab stage="stage 6" title="Analyzer" />
        </TabsContent>
        <TabsContent value="profiles">
          <PlaceholderTab stage="stage 5" title="Profiles" />
        </TabsContent>
      </Tabs>
    </div>
  );
}

export default App;
