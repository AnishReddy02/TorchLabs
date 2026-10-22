import {createContext, useContext} from 'react';

export type RunPrefill = {assetId?: string; workflowId?: string; bundleId?: string | null};
export type Actions = {openPalette: () => void; newRun: (prefill?: RunPrefill) => void; openHelp: () => void};
export const ActionsContext = createContext<Actions>({openPalette: () => {}, newRun: () => {}, openHelp: () => {}});
export const useActions = () => useContext(ActionsContext);
