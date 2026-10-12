// SPDX-License-Identifier: AGPL-3.0-only
// Qualification-only standard Qt editor; no input-method or undo overrides.
#include <QApplication>
#include <QEvent>
#include <QObject>
#include <QVariant>
#include <QFile>
#include <QGuiApplication>
#include <QPlainTextEdit>
#include <QLineEdit>
#include <QTextEdit>
#include <QString>
#include <cstdio>
class FieldFocusObserver final : public QObject {
public:
    explicit FieldFocusObserver(const QString &mode) : mode_(mode) {}
protected:
    bool eventFilter(QObject *object, QEvent *event) override {
        if (event->type() == QEvent::FocusIn) {
            const auto *widget = qobject_cast<QWidget *>(object);
            if (widget) {
                std::printf("QT_NATIVE_FIELD_FOCUS %s hints=%u im-enabled=%d\n",
                            mode_.toUtf8().constData(),
                            static_cast<unsigned int>(widget->inputMethodHints()),
                            widget->inputMethodQuery(Qt::ImEnabled).toBool() ? 1 : 0);
                std::fflush(stdout);
            }
        }
        return QObject::eventFilter(object, event);
    }
private:
    QString mode_;
};
int main(int argc, char **argv) {
    QApplication app(argc, argv);
    QGuiApplication::setDesktopFileName("so.tilo.DictationQtFixture");
    if (argc != 3) return 2;
    QFile file(QString::fromLocal8Bit(argv[2]));
    if (!file.open(QIODevice::ReadOnly)) return 3;
    const QString text = QString::fromUtf8(file.readAll());
    const QString mode = QString::fromLocal8Bit(argv[1]);
    const bool plain = mode == "plain";
    QWidget *editor;
    if (mode == "line" || mode == "password" || mode == "pin" || mode == "sensitive") {
        auto *widget = new QLineEdit;
        if (mode == "password" || mode == "pin") widget->setEchoMode(QLineEdit::Password);
        if (mode == "pin") widget->setInputMethodHints(widget->inputMethodHints() | Qt::ImhDigitsOnly);
        if (mode == "sensitive") widget->setInputMethodHints(Qt::ImhSensitiveData);
        if (mode == "line") widget->setText(text);
        editor = widget;
    } else if (plain) {
        auto *widget = new QPlainTextEdit;
        widget->setPlainText(text);
        editor = widget;
    } else {
        auto *widget = new QTextEdit;
        widget->setAcceptRichText(false);
        widget->setPlainText(text);
        editor = widget;
    }
    FieldFocusObserver observer(mode);
    editor->installEventFilter(&observer);
    editor->setWindowTitle("Dictation qualification");
    editor->resize(650, 450);
    editor->show();
    editor->setFocus();
    std::printf("QT_NATIVE_EDITOR %s Qt %s\n", mode.toUtf8().constData(), qVersion());
    std::fflush(stdout);
    const int result = app.exec();
    delete editor;
    return result;
}
