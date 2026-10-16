#!/usr/bin/env python3
"""Build remote-only ASR memory characterization fixtures; never change production limits.

Run on the existing remote builder checkout after verifying free memory/pressure.
The pinned native archives must already exist. This script downloads nothing.
"""
from pathlib import Path
import subprocess
import argparse
import hashlib
import json

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--include-2g-characterization', action='store_true',
                    help='explicitly generate separate labeled 2GiB remote probes; no production qualification')
args = parser.parse_args()
root = Path('/tmp/tilo-dictation')
out = root / 'target/asr-memory-characterization-v1'
out.mkdir(mode=0o700, exist_ok=True)
fixture = (root / 'tools/release/fixtures/asr_whisper_compare.cpp').read_text()
bridge = (root / 'crates/agent/local-model-service/native/speech.cpp').read_text()
probe = (root / 'tools/release/probe_asr_candidate.py').read_text()
if hashlib.sha256(bridge.encode()).hexdigest() != 'cbf9f788f7c19485221f3b220477f90134b649072f9e89ccdf8e2041bc8d2bc4':
    raise ValueError('pinned production bridge changed; review characterization transformations first')
if probe.count('1073741824') != 3 or probe.count('960 * 1024') != 1 or probe.count('960*1024*1024') != 1:
    raise ValueError('bounded experiment source changed; never silently change its limits')
memory = r"""
static void memory_stage(const char* stage) {
    std::ifstream status("/proc/self/status"); std::string line;
    std::printf("ASR_MEMORY_STAGE {\"stage\":\"%s\"", stage);
    while (std::getline(status,line)) {
        for (const char* key : {"VmSize", "VmRSS", "VmHWM", "VmPeak"}) {
            const auto prefix = std::string(key)+":";
            if (line.rfind(prefix,0)==0) std::printf(",\"%s_kib\":%lld",key,std::stoll(line.substr(prefix.size())));
        }
    }
    std::puts("}"); std::fflush(stdout);
}
"""
fixture=fixture.replace('static std::string json_text',memory+'\nstatic std::string json_text')
fixture=fixture.replace('void* model = tilo_speech_load', 'memory_stage("input_weights_loaded");\n    void* model = tilo_speech_load')
fixture=fixture.replace('bytes.clear(); bytes.shrink_to_fit();', 'memory_stage("model_and_input_overlap");\n    bytes.clear(); bytes.shrink_to_fit();\n    memory_stage("input_weights_released");')
fixture=fixture.replace('std::printf("ASR_LOAD', 'memory_stage("decoder_ready");\n    std::printf("ASR_LOAD')
fixture=fixture.replace('tilo_speech_free(model);\n    return 0;', 'memory_stage("final_decoded");\n    tilo_speech_free(model);\n    memory_stage("model_freed");\n    return 0;')
(out/'compare.cpp').write_text(fixture)
bridge=bridge.replace('static void discard_log(enum ggml_log_level, const char *, void *) {}', 'static void discard_log(enum ggml_log_level, const char * text, void *) { std::fputs(text, stderr); }')
if '#include <cstdio>' not in bridge:bridge='#include <cstdio>\n'+bridge
libs=root/'target/debug/build/local-model-service-6b431133136b1bd0/out/speech-install/lib'
up=root/'crates/agent/local-model-service/native/upstream/whisper.cpp'
if hashlib.sha256((libs/'libwhisper.a').read_bytes()).hexdigest() != '8edb90e579010914c37e9854ceec114d845927136636a57b14ae9bdab99cfe6c':
    raise ValueError('exact pinned native archive required; no shared/foreign build substitution')
for variant in ['baseline','flash','flash-adaptive']:
    text=bridge.replace('params.flash_attn = false;', 'params.flash_attn = true;' if variant.startswith('flash') else 'params.flash_attn = false;')
    if variant == 'flash-adaptive':
        text=text.replace('params.no_context = true;', 'params.no_context = true;\n    params.audio_ctx = std::min(1500, ((count + 319) / 320 + 255) / 256 * 256);')
    source=out/('bridge-'+variant+'.cpp');source.write_text(text)
    subprocess.run(['c++','-std=c++17','-O2','-I'+str(up/'include'),'-I'+str(up/'ggml/include'),str(out/'compare.cpp'),str(source),*[str(libs/f) for f in ['libwhisper.a','libggml.a','libggml-cpu.a','libggml-base.a']],'-lpthread','-ldl','-o',str(out/('asr-whisper-compare-'+variant))],check=True)
for variant in ['baseline','flash','flash-adaptive']:
    for budget in ([1,2] if args.include_2g_characterization else [1]):
        text=probe.replace('1073741824',str(budget*1073741824)).replace('960 * 1024',str(budget*1024-64)+' * 1024').replace('960*1024*1024',str(budget*1024-64)+'*1024*1024')
        text=text.replace("ROOT/'asr-whisper-compare-v1/asr-whisper-compare'", "ROOT/'asr-memory-characterization-v1/asr-whisper-compare-"+variant+"'")
        text=text.replace("'--setenv', 'LANG', 'C.UTF-8'", "'--setenv', 'NEMO_SPEECH_MEMSTATS', '1', '--setenv', 'LANG', 'C.UTF-8'")
        text=text.replace("profile='remote builder affinity; not physical older hardware'", "profile='EXPLICIT REMOTE CHARACTERIZATION "+str(budget)+"GiB; "+variant+"; not production qualification or physical older hardware'")
        (out/('probe-'+variant+'-'+str(budget)+'g.py')).write_text(text)
files = [out/'compare.cpp', *out.glob('bridge-*.cpp'), *out.glob('asr-whisper-compare-*'), *libs.glob('lib*.a')]
receipts = {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest() for path in files}
(out/'build-receipts.json').write_text(json.dumps(receipts, indent=2)+'\n')
print('Characterization binaries built; production source/default/cap unchanged')
