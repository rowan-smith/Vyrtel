import { fireEvent, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { ErrorBanner } from '../../src/components/common';
import { LogsSidebar } from '../../src/components/LogsSidebar';
import { LogRow } from '../../src/components/LogRow';
import { PropertyTree } from '../../src/components/PropertyTree';
import { LiveToggle, QueryBar } from '../../src/components/QueryBar';
import { ApiError } from '../../src/lib/api';
import { logEvent, renderWithRouter } from './helpers';

describe('LiveToggle', () => {
  it('is always labelled Live; the dot shows off (red) and on (green), and it toggles', async () => {
    const onChange = vi.fn();
    const { rerender } = render(<LiveToggle live={false} onChange={onChange} />);
    const btn = screen.getByRole('button', { name: 'Live' });
    expect(btn).toHaveAttribute('aria-pressed', 'false');
    expect(btn).toHaveClass('is-paused');
    await userEvent.click(btn);
    expect(onChange).toHaveBeenCalledWith(true);

    rerender(<LiveToggle live={true} status="connecting" onChange={onChange} />);
    expect(screen.getByRole('button', { name: 'Live' })).toHaveClass('is-connecting');

    rerender(<LiveToggle live={true} status="live" onChange={onChange} />);
    const live = screen.getByRole('button', { name: 'Live' });
    expect(live).toHaveAttribute('aria-pressed', 'true');
    expect(live).toHaveClass('is-live');
    await userEvent.click(live);
    expect(onChange).toHaveBeenLastCalledWith(false);
  });
});

describe('QueryBar', () => {
  it('runs the trimmed query on submit', async () => {
    const onRun = vi.fn();
    render(<QueryBar query="" onRun={onRun} />);
    const input = screen.getByRole('textbox', { name: 'Query' });
    await userEvent.type(input, '  level = Error  {Enter}');
    expect(onRun).toHaveBeenCalledWith('level = Error');
    await userEvent.click(screen.getByRole('button', { name: 'Run' }));
    expect(onRun).toHaveBeenCalledTimes(2);
  });

  it('follows external query changes', () => {
    const { rerender } = render(<QueryBar query="a = 1" onRun={() => {}} />);
    expect(screen.getByRole('textbox', { name: 'Query' })).toHaveValue('a = 1');
    rerender(<QueryBar query='service = "x"' onRun={() => {}} />);
    expect(screen.getByRole('textbox', { name: 'Query' })).toHaveValue('service = "x"');
  });
});

describe('LogRow', () => {
  it('expands vertically to show all details', async () => {
    const onToggle = vi.fn();
    const ev = logEvent();
    const { rerender } = renderWithRouter(<LogRow event={ev} expanded={false} onToggle={onToggle} />);
    const line = screen.getByRole('button', { name: /payment provider timed out/i });
    expect(line).toHaveAttribute('aria-expanded', 'false');
    expect(within(line).getByText('ERROR')).toBeInTheDocument();
    // Default columns are timestamp, level and message.
    expect(within(line).queryByText('payments')).not.toBeInTheDocument();
    expect(screen.queryByTestId('log-details')).not.toBeInTheDocument();
    await userEvent.click(line);
    expect(onToggle).toHaveBeenCalled();

    rerender(<LogRow event={ev} expanded={true} onToggle={onToggle} />);
    const details = screen.getByTestId('log-details');
    expect(within(details).getByText('4bf92f3577b34da6a3ce929d0e0e4736')).toBeInTheDocument();
    expect(within(details).getByText('00f067aa0ba902b7')).toBeInTheDocument();
    expect(within(details).getByText('TimeoutException')).toBeInTheDocument();
    expect(within(details).getByTestId('stacktrace')).toHaveTextContent('at Payments.Charge()');
    expect(within(details).getByRole('link', { name: 'View trace' })).toHaveAttribute('href', '/traces/4bf92f3577b34da6a3ce929d0e0e4736');
    // Raw event toggle.
    await userEvent.click(within(details).getByRole('button', { name: 'Show JSON' }));
    expect(within(details).getByText(/"paymentProvider": "stripe"/)).toBeInTheDocument();
  });

  it('navigates to the trace from a log (trace navigation)', async () => {
    renderWithRouter(<LogRow event={logEvent()} expanded={true} onToggle={() => {}} />, '/logs');
    await userEvent.click(screen.getByRole('link', { name: 'View trace' }));
    expect(window.location.pathname).toBe('/traces/4bf92f3577b34da6a3ce929d0e0e4736');
  });
});

describe('PropertyTree', () => {
  it('renders nested properties vertically with dotted paths', () => {
    render(<PropertyTree fields={logEvent().properties} />);
    expect(screen.getByTestId('prop-customerId')).toHaveTextContent('481');
    expect(screen.getByTestId('prop-http.statusCode')).toHaveTextContent('504');
    // Nested object rows live inside the parent row, not as new columns.
    expect(within(screen.getByTestId('prop-http')).getByTestId('prop-http.method')).toHaveTextContent('POST');
    expect(screen.getByTestId('prop-tags')).toHaveTextContent('["a","b"]');
    expect(screen.getByTestId('prop-note')).toHaveTextContent('null');
  });

  it('offers include/exclude filters with the full path', () => {
    const onFilter = vi.fn();
    render(<PropertyTree fields={{ http: { statusCode: 500 } }} onFilter={onFilter} />);
    fireEvent.click(screen.getByRole('button', { name: 'Include http.statusCode' }));
    expect(onFilter).toHaveBeenCalledWith('http.statusCode', 500, false);
    fireEvent.click(screen.getByRole('button', { name: 'Exclude http.statusCode' }));
    expect(onFilter).toHaveBeenLastCalledWith('http.statusCode', 500, true);
  });

  it('handles empty properties', () => {
    render(<PropertyTree fields={{}} />);
    expect(screen.getByText('No properties')).toBeInTheDocument();
  });
});

describe('LogsSidebar', () => {
  const facets = [
    { field: 'level', count: 100, truncated: false, values: [{ value: 'Error', count: 381 }, { value: 'Warning', count: 142 }] },
    { field: 'environment', count: 100, truncated: false, values: [{ value: 'production', count: 90 }] },
    { field: 'customerId', count: 50, truncated: false, values: [{ value: 481, count: 50 }] },
  ];
  const renderSidebar = (query: string, columns = ['timestamp', 'level', 'message']) => {
    const onFilter = vi.fn();
    const onColumnsChange = vi.fn();
    render(<LogsSidebar facets={facets} sampled={100} query={query} columns={columns} onFilter={onFilter} onColumnsChange={onColumnsChange} />);
    return { onFilter, onColumnsChange };
  };

  it('filters act like radio buttons: pick to filter, pick again to clear', async () => {
    const { onFilter } = renderSidebar('level = "Error"');
    const levels = screen.getByRole('list', { name: 'Levels' });
    expect(screen.getByText('381')).toBeInTheDocument();
    // Only levels and environments are offered as filters for now.
    expect(screen.getByRole('list', { name: 'Environments' })).toBeInTheDocument();
    expect(screen.queryByText('481')).not.toBeInTheDocument();
    const error = within(levels).getByRole('button', { name: /Error/ });
    expect(error).toHaveAttribute('aria-pressed', 'true');
    await userEvent.click(error);
    expect(onFilter).toHaveBeenCalledWith('level', null);
    await userEvent.click(within(levels).getByRole('button', { name: /Warning/ }));
    expect(onFilter).toHaveBeenLastCalledWith('level', 'Warning');
  });

  it('offers timestamp, level and message as columns', async () => {
    const { onColumnsChange } = renderSidebar('', ['timestamp', 'level']);
    const cols = screen.getByRole('list', { name: 'Columns' });
    expect(within(cols).getAllByRole('button').map((b) => b.textContent)).toEqual(['Timestamp', 'Level', 'Message']);
    expect(within(cols).getByRole('button', { name: 'Message' })).toHaveAttribute('aria-pressed', 'false');
    await userEvent.click(within(cols).getByRole('button', { name: 'Message' }));
    expect(onColumnsChange).toHaveBeenCalledWith(['timestamp', 'level', 'message']);
    await userEvent.click(within(cols).getByRole('button', { name: 'Level' }));
    expect(onColumnsChange).toHaveBeenLastCalledWith(['timestamp']);
  });
});

describe('ErrorBanner', () => {
  it('renders query errors with a caret at the error position', () => {
    render(<ErrorBanner error={new ApiError(400, 'invalid_query', "Expected expression after 'and'", 19)} query='level = "Error" and' />);
    const alert = screen.getByRole('alert');
    expect(alert).toHaveTextContent('Query error');
    expect(alert).toHaveTextContent("Expected expression after 'and'");
    expect(screen.getByLabelText('error position').textContent).toContain(`${' '.repeat(19)}^`);
  });

  it('renders generic errors and nothing when there is no error', () => {
    const { container, rerender } = render(<ErrorBanner error={null} />);
    expect(container).toBeEmptyDOMElement();
    rerender(<ErrorBanner error={new ApiError(0, 'network', 'Cannot reach the Vyrtel server.')} />);
    expect(screen.getByRole('alert')).toHaveTextContent('Cannot reach the Vyrtel server.');
  });
});
