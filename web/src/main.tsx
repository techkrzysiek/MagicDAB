import React from 'react'
import ReactDOM from 'react-dom/client'
import 'leaflet/dist/leaflet.css'
import 'flag-icons/css/flag-icons.min.css'
import './styles.css'
import App from './App'
import { ErrorBoundary } from './components/ErrorBoundary'
import { connect } from './ws'
import { initChat } from './chat'
import { initAndroid } from './android'

initChat()
initAndroid()
connect()

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <App />
    </ErrorBoundary>
  </React.StrictMode>,
)
