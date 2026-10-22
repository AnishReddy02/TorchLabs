import React from 'react';
import {createRoot} from 'react-dom/client';
import './styles/tokens.css';
import './styles/app.css';
import {App} from './app/App';
import {TorchProvider} from './lib/store';
import {upgradeLegacyUrl} from './lib/router';
import {ToastProvider} from './ui/toast';

upgradeLegacyUrl();

createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <TorchProvider>
      <ToastProvider>
        <App/>
      </ToastProvider>
    </TorchProvider>
  </React.StrictMode>
);
