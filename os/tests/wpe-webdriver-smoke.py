#!/usr/bin/env python3
"""Exercise the installed WPE engine in an isolated, headless test container.

Requires WPEWebDriver, MiniBrowser, a container init process, and a writable
HOME/XDG_RUNTIME_DIR. Use software rendering where no test GPU is available.
"""

import base64
import hashlib
import json
import struct
import subprocess
import time
import urllib.request
from urllib.error import URLError

def request(method, path, value=None):
    data = None if value is None else json.dumps(value).encode()
    req = urllib.request.Request('http://127.0.0.1:9515' + path, data=data, method=method,
                                 headers={'Content-Type': 'application/json'})
    with urllib.request.urlopen(req, timeout=40) as response:
        result = json.load(response)
    if isinstance(result.get('value'), dict) and 'error' in result['value']:
        raise RuntimeError(result)
    return result['value']

driver = subprocess.Popen(['/usr/bin/WPEWebDriver', '--port=9515'], stdout=subprocess.DEVNULL)
session = None
try:
    for _ in range(100):
        if driver.poll() is not None:
            raise RuntimeError('WebDriver exited early')
        try:
            request('GET', '/status')
            break
        except URLError:
            time.sleep(0.1)
    else:
        raise RuntimeError('WebDriver startup timed out')
    result = request('POST', '/session', {'capabilities': {'alwaysMatch': {
        'browserName': 'MiniBrowser', 'wpe:browserOptions': {
            'binary': '/usr/libexec/wpe-webkit-2.0/MiniBrowser',
            'args': ['--automation', '--headless', '--size=800x600']}}}})
    session = result['sessionId']
    prefix = '/session/' + session
    print('CAPABILITIES', json.dumps(result['capabilities']), flush=True)
    page = '<!doctype html><title>Tilo ARM browser</title><style>body{background:#183245;color:white}h1{font-size:36px}</style><h1 id="heading">Tilo ARM browser</h1><canvas id="canvas" width="32" height="32"></canvas>'
    request('POST', prefix + '/url', {'url': 'data:text/html;base64,' + base64.b64encode(page.encode()).decode()})
    assert request('GET', prefix + '/title') == 'Tilo ARM browser'
    result = request('POST', prefix + '/execute/sync', {'script': '''
        const canvas = document.getElementById('canvas');
        const ctx = canvas.getContext('2d');
        ctx.fillStyle = '#ff0000'; ctx.fillRect(0, 0, 32, 32);
        return {answer: 6*7, text: document.getElementById('heading').textContent,
                background: getComputedStyle(document.body).backgroundColor,
                pixel: Array.from(ctx.getImageData(1,1,1,1).data)};
        ''', 'args': []})
    assert result == {'answer': 42, 'text': 'Tilo ARM browser', 'background': 'rgb(24, 50, 69)', 'pixel': [255, 0, 0, 255]}, result
    print('DOM_JS_CSS_CANVAS_PASS', json.dumps(result), flush=True)
    png = base64.b64decode(request('GET', prefix + '/screenshot'), validate=True)
    assert png[:8] == b'\x89PNG\r\n\x1a\n'
    width, height = struct.unpack('>II', png[16:24])
    assert (width, height) == (800, 600), (width, height)
    print('SCREENSHOT_PASS', width, height, hashlib.sha256(png).hexdigest(), flush=True)
finally:
    try:
        if session:
            request('DELETE', '/session/' + session)
    finally:
        driver.terminate()
        try:
            driver.wait(timeout=5)
        except subprocess.TimeoutExpired:
            driver.kill()
            driver.wait()
