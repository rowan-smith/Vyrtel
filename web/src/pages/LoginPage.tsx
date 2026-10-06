import { FormEvent, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { login, setToken } from '../api';

export default function LoginPage() {

    const navigate = useNavigate();

    const [username, setUsername] = useState('admin');
    const [password, setPassword] = useState('admin');
    const [error, setError] = useState<string | null>(null);
    const [busy, setBusy] = useState(false);

    async function onSubmit(e: FormEvent) {
        e.preventDefault();

        setBusy(true);
        setError(null);

        try {
            const res = await login(username, password);
            setToken(res.token);
            navigate('/logs', {replace: true});

        } catch (err) {
            setError(err instanceof Error ? err.message : 'Login failed');

        } finally {
            setBusy(false);
        }
    }


    return (

        <div className="login-page">

            <form className="login-card" onSubmit={onSubmit}>

                <div className="login-brand">

                    <span className="brand-dot"/>

                    Observatory

                </div>

                <p className="muted" style={{margin: '0 0 1rem'}}>

                    Sign in to continue

                </p>

                <label className="login-label">

                    Username

                    <input

                        className="input"

                        value={username}

                        onChange={(e) => setUsername(e.target.value)}

                        autoComplete="username"

                        autoFocus

                    />

                </label>

                <label className="login-label">

                    Password

                    <input

                        className="input"

                        type="password"

                        value={password}

                        onChange={(e) => setPassword(e.target.value)}

                        autoComplete="current-password"

                    />

                </label>

                {error && <p className="query-error">{error}</p>}

                <button className="btn btn-primary" type="submit" disabled={busy}>

                    {busy ? 'Signing in…' : 'Sign in'}

                </button>

            </form>

        </div>

    );

}

