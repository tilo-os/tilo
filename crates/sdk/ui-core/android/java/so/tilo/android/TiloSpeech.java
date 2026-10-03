package so.tilo.android;

import android.content.Context;
import android.speech.tts.TextToSpeech;
import android.speech.tts.Voice;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Set;

/**
 * The system text-to-speech engine, for the built-in screen reader.
 *
 * <p>The engine binds asynchronously; what is said before it is ready waits
 * and is spoken once it is, and nothing is spoken if it fails to start.
 */
public final class TiloSpeech implements TextToSpeech.OnInitListener {
    private static final int PENDING = 1;
    private static final int MAX_WAITING = 32;

    private final TextToSpeech engine;
    private final List<String> waiting = new ArrayList<>();
    private int status = PENDING;
    private long utterances;

    public TiloSpeech(Context context) {
        engine = new TextToSpeech(context.getApplicationContext(), this);
    }

    @Override
    public synchronized void onInit(int result) {
        status = result;
        if (result == TextToSpeech.SUCCESS) {
            for (String text : waiting) {
                say(text, TextToSpeech.QUEUE_ADD);
            }
        }
        waiting.clear();
    }

    /** Returns false when the engine failed to start or refused the text. */
    public synchronized boolean speak(String text, boolean interrupt) {
        if (status == PENDING) {
            if (interrupt) {
                waiting.clear();
            }
            if (waiting.size() < MAX_WAITING) {
                waiting.add(text);
            }
            return true;
        }
        if (status != TextToSpeech.SUCCESS) {
            return false;
        }
        return say(text, interrupt ? TextToSpeech.QUEUE_FLUSH : TextToSpeech.QUEUE_ADD);
    }

    private boolean say(String text, int queueMode) {
        utterances++;
        return engine.speak(text, queueMode, null, "tilo-" + utterances) == TextToSpeech.SUCCESS;
    }

    public synchronized void stop() {
        waiting.clear();
        if (status == TextToSpeech.SUCCESS) {
            engine.stop();
        }
    }

    public synchronized void setRate(float rate) {
        engine.setSpeechRate(rate);
    }

    /** Select a voice by the engine's name for it, else by BCP 47 language tag. */
    public synchronized boolean setVoice(String name) {
        if (status != TextToSpeech.SUCCESS) {
            return false;
        }
        Set<Voice> voices = engine.getVoices();
        if (voices != null) {
            for (Voice voice : voices) {
                if (voice.getName().equals(name)) {
                    return engine.setVoice(voice) == TextToSpeech.SUCCESS;
                }
            }
        }
        return engine.setLanguage(Locale.forLanguageTag(name)) >= TextToSpeech.LANG_AVAILABLE;
    }

    public synchronized boolean isSpeaking() {
        return !waiting.isEmpty() || (status == TextToSpeech.SUCCESS && engine.isSpeaking());
    }

    public synchronized void shutdown() {
        waiting.clear();
        engine.shutdown();
    }
}
