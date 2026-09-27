package so.tilo.android;

import android.app.Activity;
import android.content.Context;
import android.graphics.Matrix;
import android.graphics.Rect;
import android.icu.text.BreakIterator;
import android.os.Build;
import android.text.Editable;
import android.text.InputType;
import android.text.Selection;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.CursorAnchorInfo;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.ExtractedText;
import android.view.inputmethod.ExtractedTextRequest;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;

/**
 * The view a soft keyboard types into.
 *
 * <p>NativeActivity's content view is not a text editor, so an input method
 * never connects to it. This one is, whenever the toolkit's focused widget is
 * a text field, and it forwards what the input method does to that field.
 *
 * <p>Each connection keeps its own copy of the field's text, which the input
 * method reads back for suggestions, autocorrection and deletion. The copy
 * starts from the text around the caret that the toolkit last reported and
 * follows the connection's own edits; a change the toolkit makes by itself
 * reaches the input method with the next connection.
 */
public final class TiloTextInput extends View {
    private static final int PURPOSE_PASSWORD = 1;
    private static final int PURPOSE_TERMINAL = 2;

    private static volatile TiloTextInput sInstance;

    private final InputMethodManager mInputMethods;
    /** The caret in window pixels, as the toolkit last reported it. */
    private final Rect mCaret = new Rect();
    private boolean mEnabled;
    private int mPurpose;
    /** The field's text around its selection, in UTF-16 offsets. */
    private String mSurrounding = "";
    private int mSurroundingAnchor;
    private int mSurroundingFocus;
    private Connection mConnection;

    private TiloTextInput(Context context) {
        super(context);
        mInputMethods = (InputMethodManager) context.getSystemService(Context.INPUT_METHOD_SERVICE);
        setFocusable(true);
        setFocusableInTouchMode(true);
    }

    /** Adds the text input to {@code activity}. Call after {@code super.onCreate}. */
    public static void install(Activity activity) {
        TiloTextInput view = new TiloTextInput(activity);
        // Android does not focus a zero-sized view.
        activity.addContentView(view, new ViewGroup.LayoutParams(1, 1));
        sInstance = view;
    }

    // The natives are registered by the toolkit before it calls attach().
    private static native void nativeCommit(String text);

    private static native void nativePreedit(String text);

    private static native void nativeKey(int keyCode);

    private static native void nativeDelete(int before, int after);

    // Called by the toolkit on its own thread.
    static boolean attach() {
        return sInstance != null;
    }

    static void update(
            final boolean enabled,
            final int purpose,
            final int x,
            final int y,
            final int width,
            final int height,
            final boolean restart,
            final String surrounding,
            final int anchor,
            final int focus) {
        final TiloTextInput view = sInstance;
        if (view == null) {
            return;
        }
        view.post(new Runnable() {
            @Override
            public void run() {
                int length = surrounding.length();
                view.mSurrounding = surrounding;
                view.mSurroundingAnchor = Math.max(0, Math.min(anchor, length));
                view.mSurroundingFocus = Math.max(0, Math.min(focus, length));
                view.apply(enabled, purpose, x, y, width, height, restart);
            }
        });
    }

    static void touched() {
        final TiloTextInput view = sInstance;
        if (view == null) {
            return;
        }
        view.post(new Runnable() {
            @Override
            public void run() {
                if (view.mEnabled) {
                    view.restart();
                    view.showKeyboard();
                }
            }
        });
    }

    private void apply(
            boolean enabled, int purpose, int x, int y, int width, int height, boolean restart) {
        boolean changed = enabled != mEnabled || purpose != mPurpose;
        mEnabled = enabled;
        mPurpose = purpose;
        mCaret.set(x, y, x + width, y + height);
        if (enabled) {
            if (!isFocused()) {
                requestFocus();
            }
            if (changed || restart) {
                restart();
            }
            if (changed) {
                showKeyboard();
            }
            if (mConnection != null) {
                mConnection.reportCursor(false);
            }
        } else if (changed) {
            mInputMethods.hideSoftInputFromWindow(getWindowToken(), 0);
            restart();
        }
    }

    /**
     * Starts a fresh connection. The toolkit has already settled the old
     * composition, so the old connection must not commit it again.
     */
    private void restart() {
        if (mConnection != null) {
            BaseInputConnection.removeComposingSpans(mConnection.getEditable());
            mConnection = null;
        }
        mInputMethods.restartInput(this);
    }

    private void showKeyboard() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            WindowInsetsController controller = getWindowInsetsController();
            if (controller != null) {
                controller.show(WindowInsets.Type.ime());
                return;
            }
        }
        mInputMethods.showSoftInput(this, 0);
    }

    @Override
    public boolean onCheckIsTextEditor() {
        return mEnabled;
    }

    @Override
    public InputConnection onCreateInputConnection(EditorInfo info) {
        if (!mEnabled) {
            return null;
        }
        info.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN
                | EditorInfo.IME_FLAG_NO_EXTRACT_UI
                | EditorInfo.IME_ACTION_NONE;
        switch (mPurpose) {
            case PURPOSE_PASSWORD:
                info.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_PASSWORD;
                info.imeOptions |= EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING;
                break;
            case PURPOSE_TERMINAL:
                info.inputType = InputType.TYPE_CLASS_TEXT
                        | InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
                        | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS;
                break;
            default:
                info.inputType = InputType.TYPE_CLASS_TEXT
                        | InputType.TYPE_TEXT_FLAG_MULTI_LINE
                        | InputType.TYPE_TEXT_FLAG_AUTO_CORRECT
                        | InputType.TYPE_TEXT_FLAG_CAP_SENTENCES;
                break;
        }
        mConnection = new Connection(mSurrounding, mSurroundingAnchor, mSurroundingFocus);
        info.initialSelStart = Selection.getSelectionStart(mConnection.getEditable());
        info.initialSelEnd = Selection.getSelectionEnd(mConnection.getEditable());
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            info.setInitialSurroundingText(mConnection.getEditable());
        }
        return mConnection;
    }

    private static int graphemes(CharSequence text, int start, int end) {
        if (end <= start) {
            return 0;
        }
        BreakIterator boundaries = BreakIterator.getCharacterInstance();
        boundaries.setText(text.subSequence(start, end).toString());
        int count = 0;
        while (boundaries.next() != BreakIterator.DONE) {
            count++;
        }
        return count;
    }

    private final class Connection extends BaseInputConnection {
        private final Editable mText;
        private int mBatch;
        private boolean mMonitorCursor;

        Connection(String text, int anchor, int focus) {
            super(TiloTextInput.this, true);
            mText = Editable.Factory.getInstance().newEditable(text);
            Selection.setSelection(mText, anchor, focus);
        }

        /** Input methods keep calling a connection after it was replaced. */
        private boolean live() {
            return mConnection == this && mEnabled;
        }

        @Override
        public Editable getEditable() {
            return mText;
        }

        @Override
        public boolean beginBatchEdit() {
            mBatch++;
            return true;
        }

        @Override
        public boolean endBatchEdit() {
            if (mBatch > 0) {
                mBatch--;
                if (mBatch == 0) {
                    reportSelection();
                }
            }
            return mBatch > 0;
        }

        // The toolkit always leaves its caret after inserted text, so the copy
        // does too, whatever cursor position the input method asks for.
        @Override
        public boolean commitText(CharSequence text, int newCursorPosition) {
            if (!live()) {
                return false;
            }
            super.commitText(text, 1);
            nativeCommit(text.toString());
            changed();
            return true;
        }

        @Override
        public boolean setComposingText(CharSequence text, int newCursorPosition) {
            if (!live()) {
                return false;
            }
            super.setComposingText(text, 1);
            nativePreedit(text.toString());
            changed();
            return true;
        }

        @Override
        public boolean finishComposingText() {
            if (!live()) {
                return false;
            }
            String composing = composing();
            super.finishComposingText();
            if (!composing.isEmpty()) {
                nativeCommit(composing);
            }
            changed();
            return true;
        }

        /**
         * Reopens a word for correction. The toolkit can only express that
         * for the word ending at the caret: delete it, then compose it again.
         */
        @Override
        public boolean setComposingRegion(int start, int end) {
            if (!live()) {
                return false;
            }
            int length = mText.length();
            int from = Math.max(0, Math.min(Math.min(start, end), length));
            int to = Math.max(0, Math.min(Math.max(start, end), length));
            if (from == to) {
                return finishComposingText();
            }
            if (to != Selection.getSelectionEnd(mText) || !composing().isEmpty()) {
                return false;
            }
            String word = mText.subSequence(from, to).toString();
            nativeDelete(graphemes(mText, from, to), 0);
            super.setComposingRegion(from, to);
            nativePreedit(word);
            changed();
            return true;
        }

        @Override
        public boolean deleteSurroundingText(int beforeLength, int afterLength) {
            if (!live()) {
                return false;
            }
            int before = Math.max(0, beforeLength);
            int after = Math.max(0, afterLength);
            int[] span = deletionSpan();
            int beforeChars = Math.min(before, span[0]);
            int afterChars = Math.min(after, mText.length() - span[1]);
            delete(span, beforeChars, afterChars, before - beforeChars, after - afterChars);
            return true;
        }

        @Override
        public boolean deleteSurroundingTextInCodePoints(int beforeLength, int afterLength) {
            if (!live()) {
                return false;
            }
            int[] span = deletionSpan();
            int start = span[0];
            int missingBefore = Math.max(0, beforeLength);
            while (missingBefore > 0 && start > 0) {
                start = Character.offsetByCodePoints(mText, start, -1);
                missingBefore--;
            }
            int end = span[1];
            int missingAfter = Math.max(0, afterLength);
            while (missingAfter > 0 && end < mText.length()) {
                end = Character.offsetByCodePoints(mText, end, 1);
                missingAfter--;
            }
            delete(span, span[0] - start, end - span[1], missingBefore, missingAfter);
            return true;
        }

        /** Where deletion is measured from: the selection widened to the composition. */
        private int[] deletionSpan() {
            int start = Math.min(Selection.getSelectionStart(mText), Selection.getSelectionEnd(mText));
            int end = Math.max(Selection.getSelectionStart(mText), Selection.getSelectionEnd(mText));
            int composingStart = getComposingSpanStart(mText);
            int composingEnd = getComposingSpanEnd(mText);
            if (composingStart >= 0 && composingEnd >= 0) {
                start = Math.min(start, Math.min(composingStart, composingEnd));
                end = Math.max(end, Math.max(composingStart, composingEnd));
            }
            return new int[] {Math.max(0, start), Math.max(0, end)};
        }

        /**
         * Deletes {@code beforeChars} and {@code afterChars} of the copy
         * around {@code span}, and the text the copy does not have one key
         * press per missing character.
         */
        private void delete(
                int[] span, int beforeChars, int afterChars, int missingBefore, int missingAfter) {
            int backspaces = graphemes(mText, span[0] - beforeChars, span[0]) + missingBefore;
            int deletes = graphemes(mText, span[1], span[1] + afterChars) + missingAfter;
            super.deleteSurroundingText(beforeChars, afterChars);
            if (backspaces > 0 || deletes > 0) {
                nativeDelete(backspaces, deletes);
            }
            changed();
        }

        @Override
        public boolean sendKeyEvent(KeyEvent event) {
            if (!live()) {
                return false;
            }
            if (event.getAction() != KeyEvent.ACTION_DOWN) {
                return true;
            }
            finishComposingText();
            int unicode = event.getUnicodeChar(event.getMetaState());
            if (unicode > 0 && !Character.isISOControl(unicode)) {
                String text = new String(Character.toChars(unicode));
                replaceSelection(text);
                nativeCommit(text);
                changed();
                return true;
            }
            int keyCode = event.getKeyCode();
            int caret = Selection.getSelectionEnd(mText);
            switch (keyCode) {
                case KeyEvent.KEYCODE_DEL:
                    if (caret > 0) {
                        mText.delete(precedingBoundary(caret), caret);
                    }
                    break;
                case KeyEvent.KEYCODE_FORWARD_DEL:
                    if (caret < mText.length()) {
                        mText.delete(caret, followingBoundary(caret));
                    }
                    break;
                case KeyEvent.KEYCODE_ENTER:
                case KeyEvent.KEYCODE_NUMPAD_ENTER:
                    replaceSelection("\n");
                    break;
                case KeyEvent.KEYCODE_DPAD_LEFT:
                    Selection.setSelection(mText, precedingBoundary(caret));
                    break;
                case KeyEvent.KEYCODE_DPAD_RIGHT:
                    Selection.setSelection(mText, followingBoundary(caret));
                    break;
                default:
                    break;
            }
            nativeKey(keyCode);
            changed();
            return true;
        }

        /** Moves the caret with arrow keys; the toolkit takes no selection from here. */
        @Override
        public boolean setSelection(int start, int end) {
            if (!live()) {
                return false;
            }
            finishComposingText();
            int caret = Selection.getSelectionEnd(mText);
            int target = Math.max(0, Math.min(end, mText.length()));
            int moves = graphemes(mText, Math.min(caret, target), Math.max(caret, target));
            int key = target < caret ? KeyEvent.KEYCODE_DPAD_LEFT : KeyEvent.KEYCODE_DPAD_RIGHT;
            for (int i = 0; i < moves; i++) {
                nativeKey(key);
            }
            Selection.setSelection(mText, target);
            changed();
            return true;
        }

        @Override
        public boolean performEditorAction(int editorAction) {
            if (!live()) {
                return false;
            }
            finishComposingText();
            nativeKey(KeyEvent.KEYCODE_ENTER);
            return true;
        }

        @Override
        public ExtractedText getExtractedText(ExtractedTextRequest request, int flags) {
            ExtractedText extracted = new ExtractedText();
            extracted.text = mText.toString();
            extracted.startOffset = 0;
            extracted.partialStartOffset = -1;
            extracted.partialEndOffset = -1;
            extracted.selectionStart = Selection.getSelectionStart(mText);
            extracted.selectionEnd = Selection.getSelectionEnd(mText);
            return extracted;
        }

        @Override
        public boolean requestCursorUpdates(int cursorUpdateMode) {
            mMonitorCursor = (cursorUpdateMode & InputConnection.CURSOR_UPDATE_MONITOR) != 0;
            if ((cursorUpdateMode & InputConnection.CURSOR_UPDATE_IMMEDIATE) != 0) {
                reportCursor(true);
            }
            return true;
        }

        private String composing() {
            int start = getComposingSpanStart(mText);
            int end = getComposingSpanEnd(mText);
            if (start < 0 || end < 0 || start == end) {
                return "";
            }
            return mText.subSequence(Math.min(start, end), Math.max(start, end)).toString();
        }

        private void replaceSelection(String text) {
            int start = Math.min(Selection.getSelectionStart(mText), Selection.getSelectionEnd(mText));
            int end = Math.max(Selection.getSelectionStart(mText), Selection.getSelectionEnd(mText));
            mText.replace(Math.max(0, start), Math.max(0, end), text);
        }

        private int precedingBoundary(int offset) {
            BreakIterator boundaries = BreakIterator.getCharacterInstance();
            boundaries.setText(mText.toString());
            int boundary = boundaries.preceding(offset);
            return boundary == BreakIterator.DONE ? 0 : boundary;
        }

        private int followingBoundary(int offset) {
            BreakIterator boundaries = BreakIterator.getCharacterInstance();
            boundaries.setText(mText.toString());
            int boundary = boundaries.following(offset);
            return boundary == BreakIterator.DONE ? mText.length() : boundary;
        }

        private void changed() {
            if (mBatch == 0) {
                reportSelection();
            }
        }

        private void reportSelection() {
            mInputMethods.updateSelection(
                    TiloTextInput.this,
                    Selection.getSelectionStart(mText),
                    Selection.getSelectionEnd(mText),
                    getComposingSpanStart(mText),
                    getComposingSpanEnd(mText));
            reportCursor(false);
        }

        void reportCursor(boolean immediate) {
            if (!immediate && !mMonitorCursor) {
                return;
            }
            CursorAnchorInfo.Builder info = new CursorAnchorInfo.Builder();
            info.setSelectionRange(Selection.getSelectionStart(mText), Selection.getSelectionEnd(mText));
            int composingStart = getComposingSpanStart(mText);
            String composing = composing();
            if (!composing.isEmpty()) {
                info.setComposingText(composingStart, composing);
            }
            info.setInsertionMarkerLocation(
                    mCaret.left,
                    mCaret.top,
                    mCaret.bottom,
                    mCaret.bottom,
                    CursorAnchorInfo.FLAG_HAS_VISIBLE_REGION);
            // The native surface fills the window, so the toolkit's caret is
            // in window coordinates rather than this view's.
            int[] origin = new int[2];
            getRootView().getLocationOnScreen(origin);
            Matrix toScreen = new Matrix();
            toScreen.setTranslate(origin[0], origin[1]);
            info.setMatrix(toScreen);
            mInputMethods.updateCursorAnchorInfo(TiloTextInput.this, info.build());
        }
    }
}
