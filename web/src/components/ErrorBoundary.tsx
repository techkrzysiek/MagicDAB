import React from 'react'
import { reportUiError } from '../ws'

/** Shows the error instead of a blank page and reports it to the server log. */
export class ErrorBoundary extends React.Component<{ children: React.ReactNode }, { error: Error | null }> {
  state = { error: null as Error | null }

  static getDerivedStateFromError(error: Error) {
    return { error }
  }

  componentDidCatch(error: Error, info: React.ErrorInfo) {
    reportUiError(error.message, `${error.stack ?? ''}\n${info.componentStack ?? ''}`)
  }

  render() {
    if (this.state.error) {
      return (
        <div className="crash">
          <h3>MagicDAB: błąd interfejsu / interface error</h3>
          <pre>{this.state.error.message}</pre>
          <button className="btn primary" onClick={() => location.reload()}>
            Przeładuj / Reload
          </button>
        </div>
      )
    }
    return this.props.children
  }
}
