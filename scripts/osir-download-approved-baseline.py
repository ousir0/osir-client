"""Read the exact published 0.1.8 baseline, accepting one approved redirect."""
import hashlib
from pathlib import Path
import sys
import urllib.error
import urllib.request

BASELINES = {
    'windows-x86_64': ('x86_64-pc-windows-msvc-setup.exe', 15879325,
        '644c20ec43233f9eca8c53cc62bb4f8a9f65e94071796e1dde7f64d16b89b382'),
    'darwin-x86_64': ('x86_64-apple-darwin.app.tar.gz', 21043942,
        '928203d32218b5bfd3c687f8fbf6a54f10de105d5a848b84f2e687badef4c581'),
}

class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None

def main():
    suffix, size, digest = BASELINES[sys.argv[1]]
    target = Path(sys.argv[2])
    assert not target.exists(), 'Refusing to overwrite baseline'
    name = 'OSIR-Client-prod-0.1.8-' + suffix
    source = 'https://osirclaw.com/api/v1/osir-client/artifacts/prod-0.1.8/' + name
    expected = 'https://osirvedio.cn-nb1.rains3.com/osir-client/releases/prod-0.1.8/' + name
    opener = urllib.request.build_opener(NoRedirect)
    try:
        opener.open(source, timeout=90)
        raise AssertionError('Expected approved object-storage redirect')
    except urllib.error.HTTPError as response:
        assert response.code == 302 and response.headers['Location'] == expected
    checksum = hashlib.sha256()
    observed = 0
    with opener.open(expected, timeout=90) as response, target.open('xb') as output:
        assert response.status == 200 and int(response.headers['Content-Length']) == size
        while block := response.read(1024 * 1024):
            observed += len(block)
            assert observed <= size
            checksum.update(block)
            output.write(block)
    assert observed == size and checksum.hexdigest() == digest, 'Published baseline bytes differ'
    print('Verified exact published 0.1.8 baseline:', sys.argv[1], observed, digest)

if __name__ == '__main__':
    main()
