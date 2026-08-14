# Build environment. Source it: `source env.sh`
#
# JAVA_HOME IS DETECTED, NOT HARDCODED, because this checkout is built from two places: the
# host (~/dev/grouse) and the goose container, which mounts it read-write at /workspace/grouse
# and builds the APK itself. Their JDKs are in different places -- $HOME/.jdk17 in the
# container, a distro OpenJDK under /usr/lib/jvm on the host -- so a hardcoded path is right
# for whoever edited it last and broken for the other. It has been both, in that order.
#
# Order of preference: an already-valid JAVA_HOME (someone who set it meant it), a real JDK
# (javac present), then a bare JRE.
#
# A JRE IS ENOUGH HERE and that is not an accident worth breaking: the app is entirely Kotlin,
# so javac is never invoked -- kotlinc does the compiling and Gradle only needs a JVM to run
# on. The host has exactly that (a headless JRE 17, no javac anywhere) and builds fine, so an
# `-x javac` test would reject a working machine. A real JDK is still preferred where one
# exists, because a stricter toolchain requirement later should not silently pick the JRE.

# The -n guard is not redundant: with an empty argument this tests "/bin/java", which exists on
# any machine with java on the PATH -- so an UNSET JAVA_HOME tested as valid and skipped the
# detection below entirely, leaving it unset. Cost half an hour.
_java_ok() { [ -n "$1" ] && [ -x "$1/bin/java" ]; }

if ! _java_ok "${JAVA_HOME:-}"; then
  for _cand in "$HOME/.jdk17" /usr/lib/jvm/java-17-openjdk* /usr/lib/jvm/java-17* \
               /usr/lib/jvm/jre-17-openjdk* /usr/lib/jvm/jre-17*; do
    if [ -x "$_cand/bin/javac" ]; then JAVA_HOME="$_cand"; break; fi
    if [ -z "${_fallback:-}" ] && _java_ok "$_cand"; then _fallback="$_cand"; fi
  done
  [ -z "${JAVA_HOME:-}" ] && JAVA_HOME="${_fallback:-}"
  unset _cand _fallback
fi

if _java_ok "${JAVA_HOME:-}"; then
  export JAVA_HOME
else
  echo "env.sh: no Java 17 found (looked in \$JAVA_HOME, ~/.jdk17, /usr/lib/jvm)." >&2
  echo "        Install one, or set JAVA_HOME before sourcing this." >&2
fi

export ANDROID_HOME=$HOME/Android/Sdk
export PATH=$JAVA_HOME/bin:$ANDROID_HOME/cmdline-tools/latest/bin:$ANDROID_HOME/platform-tools:$PATH
