// Exercise context creation, rendering, and pixel readback with either ABI.
#include <GL/gl.h>
#include <GL/glx.h>
#include <X11/Xlib.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void require(int condition, const char *operation) {
    if (!condition) {
        fprintf(stderr, "%s failed\n", operation);
        exit(1);
    }
}

int main(int argc, char **argv) {
    int window_mode = argc == 2 && strcmp(argv[1], "--window") == 0;
    require(argc == 1 || window_mode, "usage: probe [--window]");
    Display *display = XOpenDisplay(NULL);
    require(display != NULL, "XOpenDisplay");
    const int attributes[] = {
        GLX_X_RENDERABLE, True, GLX_DRAWABLE_TYPE,
        window_mode ? GLX_WINDOW_BIT : GLX_PBUFFER_BIT,
        GLX_RENDER_TYPE, GLX_RGBA_BIT, GLX_RED_SIZE, 8, GLX_GREEN_SIZE, 8,
        GLX_BLUE_SIZE, 8, GLX_ALPHA_SIZE, 8, None,
    };
    int count = 0;
    GLXFBConfig *configs = glXChooseFBConfig(display, DefaultScreen(display), attributes, &count);
    require(configs != NULL && count > 0, "glXChooseFBConfig");
    XVisualInfo *visual = glXGetVisualFromFBConfig(display, configs[0]);
    require(visual != NULL, "glXGetVisualFromFBConfig");
    fprintf(stderr, "GLX visual depth=%d screen depth=%d\n", visual->depth,
            DefaultDepth(display, DefaultScreen(display)));
    const int fields[] = {GLX_RED_SIZE, GLX_GREEN_SIZE, GLX_BLUE_SIZE, GLX_ALPHA_SIZE,
                          GLX_BUFFER_SIZE, GLX_DOUBLEBUFFER};
    for (size_t i = 0; i < sizeof(fields) / sizeof(fields[0]); ++i) {
        int value = -1;
        require(glXGetFBConfigAttrib(display, configs[0], fields[i], &value) == Success,
                "glXGetFBConfigAttrib");
        fprintf(stderr, "GLX attribute %d=%d\n", fields[i], value);
    }
    const int size[] = {GLX_PBUFFER_WIDTH, 16, GLX_PBUFFER_HEIGHT, 16, None};
    Window window = None;
    Colormap colormap = None;
    GLXDrawable surface;
    if (window_mode) {
        Window root = RootWindow(display, visual->screen);
        colormap = XCreateColormap(display, root, visual->visual, AllocNone);
        XSetWindowAttributes window_attributes = {.colormap = colormap};
        window = XCreateWindow(display, root, 0, 0, 16, 16, 0, visual->depth,
                               InputOutput, visual->visual, CWColormap, &window_attributes);
        require(window != None, "XCreateWindow");
        XMapWindow(display, window);
        surface = glXCreateWindow(display, configs[0], window, NULL);
        require(surface != None, "glXCreateWindow");
    } else {
        surface = glXCreatePbuffer(display, configs[0], size);
        require(surface != None, "glXCreatePbuffer");
    }
    XFree(visual);
    GLXContext context = glXCreateNewContext(display, configs[0], GLX_RGBA_TYPE, NULL, True);
    require(context != NULL, "glXCreateNewContext");
    XFree(configs);
    require(glXMakeContextCurrent(display, surface, surface, context), "glXMakeContextCurrent");
    glViewport(0, 0, 16, 16);
    glClearColor(0.25f, 0.5f, 0.75f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    unsigned char pixels[16 * 16 * 4] = {0};
    glReadPixels(0, 0, 16, 16, GL_RGBA, GL_UNSIGNED_BYTE, pixels);
    require(glGetError() == GL_NO_ERROR, "OpenGL clear/readback");
    const int expected[] = {64, 128, 191, 255};
    for (size_t i = 0; i < sizeof(pixels); ++i) {
        if (abs((int)pixels[i] - expected[i % 4]) > 1) {
            fprintf(stderr, "Pixel channel %zu: got %u, expected %d (+/-1)\n",
                    i, pixels[i], expected[i % 4]);
            return 1;
        }
    }
    glFinish();
    XSync(display, False);
    const char *renderer = (const char *)glGetString(GL_RENDERER);
    printf("GL_PIXELS_VERIFIED surface=%s renderer=%s pixels=256\n",
           window_mode ? "window" : "pbuffer", renderer);
    require(glXMakeContextCurrent(display, None, None, NULL), "glXMakeContextCurrent release");
    glXDestroyContext(display, context);
    if (window_mode) {
        glXDestroyWindow(display, surface);
        XDestroyWindow(display, window);
        XFreeColormap(display, colormap);
    } else {
        glXDestroyPbuffer(display, surface);
    }
    XCloseDisplay(display);
    puts("TILO_GL_PIXELS_PASS");
    return 0;
}
