use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

const ZSH_INTEGRATION: &str = include_str!("../../../shell/ghost-complete.zsh");

const EMBEDDED_SPECS: &[(&str, &str)] = &[
    ("-.json", include_str!("../../../specs/-.json")),
    ("act.json", include_str!("../../../specs/act.json")),
    ("adb.json", include_str!("../../../specs/adb.json")),
    ("adr.json", include_str!("../../../specs/adr.json")),
    ("afplay.json", include_str!("../../../specs/afplay.json")),
    ("aftman.json", include_str!("../../../specs/aftman.json")),
    ("ag.json", include_str!("../../../specs/ag.json")),
    ("agrippa.json", include_str!("../../../specs/agrippa.json")),
    ("airflow.json", include_str!("../../../specs/airflow.json")),
    ("aliases.json", include_str!("../../../specs/aliases.json")),
    ("amplify.json", include_str!("../../../specs/amplify.json")),
    ("ampx.json", include_str!("../../../specs/ampx.json")),
    (
        "ansible-config.json",
        include_str!("../../../specs/ansible-config.json"),
    ),
    (
        "ansible-doc.json",
        include_str!("../../../specs/ansible-doc.json"),
    ),
    (
        "ansible-galaxy.json",
        include_str!("../../../specs/ansible-galaxy.json"),
    ),
    (
        "ansible-lint.json",
        include_str!("../../../specs/ansible-lint.json"),
    ),
    (
        "ansible-playbook.json",
        include_str!("../../../specs/ansible-playbook.json"),
    ),
    ("ansible.json", include_str!("../../../specs/ansible.json")),
    ("ant.json", include_str!("../../../specs/ant.json")),
    (
        "appwrite.json",
        include_str!("../../../specs/appwrite.json"),
    ),
    ("apt.json", include_str!("../../../specs/apt.json")),
    ("arch.json", include_str!("../../../specs/arch.json")),
    (
        "arduino-cli.json",
        include_str!("../../../specs/arduino-cli.json"),
    ),
    ("argo.json", include_str!("../../../specs/argo.json")),
    ("asar.json", include_str!("../../../specs/asar.json")),
    (
        "asciinema.json",
        include_str!("../../../specs/asciinema.json"),
    ),
    ("asdf.json", include_str!("../../../specs/asdf.json")),
    ("asr.json", include_str!("../../../specs/asr.json")),
    ("assimp.json", include_str!("../../../specs/assimp.json")),
    ("astro.json", include_str!("../../../specs/astro.json")),
    ("atlas.json", include_str!("../../../specs/atlas.json")),
    ("atuin.json", include_str!("../../../specs/atuin.json")),
    (
        "authchanger.json",
        include_str!("../../../specs/authchanger.json"),
    ),
    (
        "autocannon.json",
        include_str!("../../../specs/autocannon.json"),
    ),
    (
        "autojump.json",
        include_str!("../../../specs/autojump.json"),
    ),
    (
        "aws-vault.json",
        include_str!("../../../specs/aws-vault.json"),
    ),
    ("awsume.json", include_str!("../../../specs/awsume.json")),
    ("babel.json", include_str!("../../../specs/babel.json")),
    ("banner.json", include_str!("../../../specs/banner.json")),
    (
        "barnard59.json",
        include_str!("../../../specs/barnard59.json"),
    ),
    ("base32.json", include_str!("../../../specs/base32.json")),
    ("base64.json", include_str!("../../../specs/base64.json")),
    (
        "basename.json",
        include_str!("../../../specs/basename.json"),
    ),
    ("basenc.json", include_str!("../../../specs/basenc.json")),
    ("bat.json", include_str!("../../../specs/bat.json")),
    ("bazel.json", include_str!("../../../specs/bazel.json")),
    ("bc.json", include_str!("../../../specs/bc.json")),
    ("bcd.json", include_str!("../../../specs/bcd.json")),
    ("bit.json", include_str!("../../../specs/bit.json")),
    ("black.json", include_str!("../../../specs/black.json")),
    ("blitz.json", include_str!("../../../specs/blitz.json")),
    ("bosh.json", include_str!("../../../specs/bosh.json")),
    ("br.json", include_str!("../../../specs/br.json")),
    ("brew.json", include_str!("../../../specs/brew.json")),
    ("broot.json", include_str!("../../../specs/broot.json")),
    (
        "browser-sync.json",
        include_str!("../../../specs/browser-sync.json"),
    ),
    ("btop.json", include_str!("../../../specs/btop.json")),
    (
        "build-storybook.json",
        include_str!("../../../specs/build-storybook.json"),
    ),
    ("bun.json", include_str!("../../../specs/bun.json")),
    ("bundle.json", include_str!("../../../specs/bundle.json")),
    ("bunx.json", include_str!("../../../specs/bunx.json")),
    ("bw.json", include_str!("../../../specs/bw.json")),
    ("bwdc.json", include_str!("../../../specs/bwdc.json")),
    ("bws.json", include_str!("../../../specs/bws.json")),
    ("c++.json", include_str!("../../../specs/c++.json")),
    (
        "caffeinate.json",
        include_str!("../../../specs/caffeinate.json"),
    ),
    ("cal.json", include_str!("../../../specs/cal.json")),
    ("cap.json", include_str!("../../../specs/cap.json")),
    (
        "capacitor.json",
        include_str!("../../../specs/capacitor.json"),
    ),
    ("cargo.json", include_str!("../../../specs/cargo.json")),
    ("cat.json", include_str!("../../../specs/cat.json")),
    ("cci.json", include_str!("../../../specs/cci.json")),
    ("cd.json", include_str!("../../../specs/cd.json")),
    ("cdk.json", include_str!("../../../specs/cdk.json")),
    ("cdk8s.json", include_str!("../../../specs/cdk8s.json")),
    ("cf.json", include_str!("../../../specs/cf.json")),
    ("charm.json", include_str!("../../../specs/charm.json")),
    ("checkov.json", include_str!("../../../specs/checkov.json")),
    ("chezmoi.json", include_str!("../../../specs/chezmoi.json")),
    ("chmod.json", include_str!("../../../specs/chmod.json")),
    ("chown.json", include_str!("../../../specs/chown.json")),
    ("chsh.json", include_str!("../../../specs/chsh.json")),
    ("cicada.json", include_str!("../../../specs/cicada.json")),
    (
        "circleci.json",
        include_str!("../../../specs/circleci.json"),
    ),
    ("clang.json", include_str!("../../../specs/clang.json")),
    ("clang++.json", include_str!("../../../specs/clang++.json")),
    ("clear.json", include_str!("../../../specs/clear.json")),
    ("claude.json", include_str!("../../../specs/claude.json")),
    (
        "cliff-jumper.json",
        include_str!("../../../specs/cliff-jumper.json"),
    ),
    ("clilol.json", include_str!("../../../specs/clilol.json")),
    ("clion.json", include_str!("../../../specs/clion.json")),
    ("clojure.json", include_str!("../../../specs/clojure.json")),
    (
        "cloudflared.json",
        include_str!("../../../specs/cloudflared.json"),
    ),
    ("cmake.json", include_str!("../../../specs/cmake.json")),
    ("coda.json", include_str!("../../../specs/coda.json")),
    (
        "code-insiders.json",
        include_str!("../../../specs/code-insiders.json"),
    ),
    ("code.json", include_str!("../../../specs/code.json")),
    (
        "codesign.json",
        include_str!("../../../specs/codesign.json"),
    ),
    ("codex.json", include_str!("../../../specs/codex.json")),
    ("command.json", include_str!("../../../specs/command.json")),
    (
        "composer.json",
        include_str!("../../../specs/composer.json"),
    ),
    ("conda.json", include_str!("../../../specs/conda.json")),
    ("copilot.json", include_str!("../../../specs/copilot.json")),
    (
        "copyfile.json",
        include_str!("../../../specs/copyfile.json"),
    ),
    (
        "copypath.json",
        include_str!("../../../specs/copypath.json"),
    ),
    ("cordova.json", include_str!("../../../specs/cordova.json")),
    ("cosign.json", include_str!("../../../specs/cosign.json")),
    ("cot.json", include_str!("../../../specs/cot.json")),
    ("cp.json", include_str!("../../../specs/cp.json")),
    (
        "create-completion-spec.json",
        include_str!("../../../specs/create-completion-spec.json"),
    ),
    (
        "create-next-app.json",
        include_str!("../../../specs/create-next-app.json"),
    ),
    (
        "create-nx-workspace.json",
        include_str!("../../../specs/create-nx-workspace.json"),
    ),
    (
        "create-react-app.json",
        include_str!("../../../specs/create-react-app.json"),
    ),
    (
        "create-react-native-app.json",
        include_str!("../../../specs/create-react-native-app.json"),
    ),
    (
        "create-redwood-app.json",
        include_str!("../../../specs/create-redwood-app.json"),
    ),
    (
        "create-remix.json",
        include_str!("../../../specs/create-remix.json"),
    ),
    (
        "create-t3-app.json",
        include_str!("../../../specs/create-t3-app.json"),
    ),
    (
        "create-video.json",
        include_str!("../../../specs/create-video.json"),
    ),
    (
        "create-vite.json",
        include_str!("../../../specs/create-vite.json"),
    ),
    (
        "create-web3-frontend.json",
        include_str!("../../../specs/create-web3-frontend.json"),
    ),
    ("croc.json", include_str!("../../../specs/croc.json")),
    ("crontab.json", include_str!("../../../specs/crontab.json")),
    ("csdx.json", include_str!("../../../specs/csdx.json")),
    ("curl.json", include_str!("../../../specs/curl.json")),
    ("cut.json", include_str!("../../../specs/cut.json")),
    ("cw.json", include_str!("../../../specs/cw.json")),
    ("dapr.json", include_str!("../../../specs/dapr.json")),
    ("dart.json", include_str!("../../../specs/dart.json")),
    ("date.json", include_str!("../../../specs/date.json")),
    ("dateseq.json", include_str!("../../../specs/dateseq.json")),
    ("datree.json", include_str!("../../../specs/datree.json")),
    ("dbt.json", include_str!("../../../specs/dbt.json")),
    ("dcli.json", include_str!("../../../specs/dcli.json")),
    ("dd.json", include_str!("../../../specs/dd.json")),
    ("ddev.json", include_str!("../../../specs/ddev.json")),
    ("ddosify.json", include_str!("../../../specs/ddosify.json")),
    (
        "defaultbrowser.json",
        include_str!("../../../specs/defaultbrowser.json"),
    ),
    (
        "defaults.json",
        include_str!("../../../specs/defaults.json"),
    ),
    ("degit.json", include_str!("../../../specs/degit.json")),
    ("deno.json", include_str!("../../../specs/deno.json")),
    (
        "deployctl.json",
        include_str!("../../../specs/deployctl.json"),
    ),
    ("deta.json", include_str!("../../../specs/deta.json")),
    ("df.json", include_str!("../../../specs/df.json")),
    ("diff.json", include_str!("../../../specs/diff.json")),
    ("dig.json", include_str!("../../../specs/dig.json")),
    ("direnv.json", include_str!("../../../specs/direnv.json")),
    ("dirname.json", include_str!("../../../specs/dirname.json")),
    ("ditto.json", include_str!("../../../specs/ditto.json")),
    (
        "django-admin.json",
        include_str!("../../../specs/django-admin.json"),
    ),
    (
        "do-release-upgrade.json",
        include_str!("../../../specs/do-release-upgrade.json"),
    ),
    ("do.json", include_str!("../../../specs/do.json")),
    (
        "docker-compose.json",
        include_str!("../../../specs/docker-compose.json"),
    ),
    ("docker.json", include_str!("../../../specs/docker.json")),
    ("doctl.json", include_str!("../../../specs/doctl.json")),
    ("dog.json", include_str!("../../../specs/dog.json")),
    ("doggo.json", include_str!("../../../specs/doggo.json")),
    (
        "dos2unix.json",
        include_str!("../../../specs/dos2unix.json"),
    ),
    (
        "dotenv-vault.json",
        include_str!("../../../specs/dotenv-vault.json"),
    ),
    ("dotenv.json", include_str!("../../../specs/dotenv.json")),
    ("dotnet.json", include_str!("../../../specs/dotnet.json")),
    (
        "dotslash.json",
        include_str!("../../../specs/dotslash.json"),
    ),
    ("dpkg.json", include_str!("../../../specs/dpkg.json")),
    ("dprint.json", include_str!("../../../specs/dprint.json")),
    ("drush.json", include_str!("../../../specs/drush.json")),
    (
        "dscacheutil.json",
        include_str!("../../../specs/dscacheutil.json"),
    ),
    ("dscl.json", include_str!("../../../specs/dscl.json")),
    ("dtm.json", include_str!("../../../specs/dtm.json")),
    ("du.json", include_str!("../../../specs/du.json")),
    ("dust.json", include_str!("../../../specs/dust.json")),
    ("eas.json", include_str!("../../../specs/eas.json")),
    ("eb.json", include_str!("../../../specs/eb.json")),
    ("echo.json", include_str!("../../../specs/echo.json")),
    (
        "electron.json",
        include_str!("../../../specs/electron.json"),
    ),
    (
        "eleventy.json",
        include_str!("../../../specs/eleventy.json"),
    ),
    ("elif.json", include_str!("../../../specs/elif.json")),
    ("elixir.json", include_str!("../../../specs/elixir.json")),
    (
        "elm-format.json",
        include_str!("../../../specs/elm-format.json"),
    ),
    (
        "elm-json.json",
        include_str!("../../../specs/elm-json.json"),
    ),
    (
        "elm-review.json",
        include_str!("../../../specs/elm-review.json"),
    ),
    ("elm.json", include_str!("../../../specs/elm.json")),
    ("else.json", include_str!("../../../specs/else.json")),
    ("emacs.json", include_str!("../../../specs/emacs.json")),
    ("enapter.json", include_str!("../../../specs/enapter.json")),
    ("encore.json", include_str!("../../../specs/encore.json")),
    ("env.json", include_str!("../../../specs/env.json")),
    (
        "envchain.json",
        include_str!("../../../specs/envchain.json"),
    ),
    ("esbuild.json", include_str!("../../../specs/esbuild.json")),
    ("eslint.json", include_str!("../../../specs/eslint.json")),
    ("exa.json", include_str!("../../../specs/exa.json")),
    ("exec.json", include_str!("../../../specs/exec.json")),
    (
        "exercism.json",
        include_str!("../../../specs/exercism.json"),
    ),
    (
        "expo-cli.json",
        include_str!("../../../specs/expo-cli.json"),
    ),
    ("expo.json", include_str!("../../../specs/expo.json")),
    ("export.json", include_str!("../../../specs/export.json")),
    (
        "expressots.json",
        include_str!("../../../specs/expressots.json"),
    ),
    ("eza.json", include_str!("../../../specs/eza.json")),
    (
        "fastlane.json",
        include_str!("../../../specs/fastlane.json"),
    ),
    ("fastly.json", include_str!("../../../specs/fastly.json")),
    ("fd.json", include_str!("../../../specs/fd.json")),
    ("fdisk.json", include_str!("../../../specs/fdisk.json")),
    ("ffmpeg.json", include_str!("../../../specs/ffmpeg.json")),
    ("figterm.json", include_str!("../../../specs/figterm.json")),
    ("file.json", include_str!("../../../specs/file.json")),
    ("find.json", include_str!("../../../specs/find.json")),
    (
        "firebase.json",
        include_str!("../../../specs/firebase.json"),
    ),
    ("firefox.json", include_str!("../../../specs/firefox.json")),
    ("fisher.json", include_str!("../../../specs/fisher.json")),
    ("flutter.json", include_str!("../../../specs/flutter.json")),
    ("fly.json", include_str!("../../../specs/fly.json")),
    ("flyctl.json", include_str!("../../../specs/flyctl.json")),
    ("fmt.json", include_str!("../../../specs/fmt.json")),
    ("fnm.json", include_str!("../../../specs/fnm.json")),
    ("fold.json", include_str!("../../../specs/fold.json")),
    ("for.json", include_str!("../../../specs/for.json")),
    ("forc.json", include_str!("../../../specs/forc.json")),
    ("forge.json", include_str!("../../../specs/forge.json")),
    ("fvm.json", include_str!("../../../specs/fvm.json")),
    (
        "fzf-tmux.json",
        include_str!("../../../specs/fzf-tmux.json"),
    ),
    ("fzf.json", include_str!("../../../specs/fzf.json")),
    ("g++.json", include_str!("../../../specs/g++.json")),
    (
        "ganache-cli.json",
        include_str!("../../../specs/ganache-cli.json"),
    ),
    ("gatsby.json", include_str!("../../../specs/gatsby.json")),
    ("gcc.json", include_str!("../../../specs/gcc.json")),
    ("gem.json", include_str!("../../../specs/gem.json")),
    ("gh.json", include_str!("../../../specs/gh.json")),
    ("ghq.json", include_str!("../../../specs/ghq.json")),
    (
        "ghost-complete.json",
        include_str!("../../../specs/ghost-complete.json"),
    ),
    ("gibo.json", include_str!("../../../specs/gibo.json")),
    (
        "git-cliff.json",
        include_str!("../../../specs/git-cliff.json"),
    ),
    (
        "git-flow.json",
        include_str!("../../../specs/git-flow.json"),
    ),
    (
        "git-profile.json",
        include_str!("../../../specs/git-profile.json"),
    ),
    (
        "git-quick-stats.json",
        include_str!("../../../specs/git-quick-stats.json"),
    ),
    ("git.json", include_str!("../../../specs/git.json")),
    ("github.json", include_str!("../../../specs/github.json")),
    ("glow.json", include_str!("../../../specs/glow.json")),
    ("gltfjsx.json", include_str!("../../../specs/gltfjsx.json")),
    ("go.json", include_str!("../../../specs/go.json")),
    ("goctl.json", include_str!("../../../specs/goctl.json")),
    ("goland.json", include_str!("../../../specs/goland.json")),
    ("googler.json", include_str!("../../../specs/googler.json")),
    (
        "goreleaser.json",
        include_str!("../../../specs/goreleaser.json"),
    ),
    ("goto.json", include_str!("../../../specs/goto.json")),
    ("gource.json", include_str!("../../../specs/gource.json")),
    ("gpg.json", include_str!("../../../specs/gpg.json")),
    ("gradle.json", include_str!("../../../specs/gradle.json")),
    ("gradlew.json", include_str!("../../../specs/gradlew.json")),
    (
        "graphcdn.json",
        include_str!("../../../specs/graphcdn.json"),
    ),
    ("grep.json", include_str!("../../../specs/grep.json")),
    ("grex.json", include_str!("../../../specs/grex.json")),
    ("gron.json", include_str!("../../../specs/gron.json")),
    ("gt.json", include_str!("../../../specs/gt.json")),
    ("gum.json", include_str!("../../../specs/gum.json")),
    ("hardhat.json", include_str!("../../../specs/hardhat.json")),
    ("hasura.json", include_str!("../../../specs/hasura.json")),
    (
        "hb-service.json",
        include_str!("../../../specs/hb-service.json"),
    ),
    ("head.json", include_str!("../../../specs/head.json")),
    ("helm.json", include_str!("../../../specs/helm.json")),
    (
        "helmfile.json",
        include_str!("../../../specs/helmfile.json"),
    ),
    ("herd.json", include_str!("../../../specs/herd.json")),
    ("hexo.json", include_str!("../../../specs/hexo.json")),
    ("homey.json", include_str!("../../../specs/homey.json")),
    ("hop.json", include_str!("../../../specs/hop.json")),
    (
        "hostname.json",
        include_str!("../../../specs/hostname.json"),
    ),
    ("htop.json", include_str!("../../../specs/htop.json")),
    ("http.json", include_str!("../../../specs/http.json")),
    ("https.json", include_str!("../../../specs/https.json")),
    ("httpy.json", include_str!("../../../specs/httpy.json")),
    ("hugo.json", include_str!("../../../specs/hugo.json")),
    ("hx.json", include_str!("../../../specs/hx.json")),
    ("hyper.json", include_str!("../../../specs/hyper.json")),
    (
        "hyperfine.json",
        include_str!("../../../specs/hyperfine.json"),
    ),
    ("ibus.json", include_str!("../../../specs/ibus.json")),
    ("iconv.json", include_str!("../../../specs/iconv.json")),
    ("id.json", include_str!("../../../specs/id.json")),
    ("idea.json", include_str!("../../../specs/idea.json")),
    ("iex.json", include_str!("../../../specs/iex.json")),
    ("if.json", include_str!("../../../specs/if.json")),
    (
        "ignite-cli.json",
        include_str!("../../../specs/ignite-cli.json"),
    ),
    ("index.json", include_str!("../../../specs/index.json")),
    ("install.json", include_str!("../../../specs/install.json")),
    ("ionic.json", include_str!("../../../specs/ionic.json")),
    ("ipatool.json", include_str!("../../../specs/ipatool.json")),
    ("j.json", include_str!("../../../specs/j.json")),
    ("java.json", include_str!("../../../specs/java.json")),
    ("jenv.json", include_str!("../../../specs/jenv.json")),
    ("jest.json", include_str!("../../../specs/jest.json")),
    ("jmeter.json", include_str!("../../../specs/jmeter.json")),
    ("join.json", include_str!("../../../specs/join.json")),
    ("jq.json", include_str!("../../../specs/jq.json")),
    ("julia.json", include_str!("../../../specs/julia.json")),
    ("jupyter.json", include_str!("../../../specs/jupyter.json")),
    ("just.json", include_str!("../../../specs/just.json")),
    ("k3d.json", include_str!("../../../specs/k3d.json")),
    ("k6.json", include_str!("../../../specs/k6.json")),
    ("k9s.json", include_str!("../../../specs/k9s.json")),
    (
        "kafkactl.json",
        include_str!("../../../specs/kafkactl.json"),
    ),
    ("kamal.json", include_str!("../../../specs/kamal.json")),
    ("kdoctor.json", include_str!("../../../specs/kdoctor.json")),
    ("keytool.json", include_str!("../../../specs/keytool.json")),
    ("kill.json", include_str!("../../../specs/kill.json")),
    ("killall.json", include_str!("../../../specs/killall.json")),
    ("kind.json", include_str!("../../../specs/kind.json")),
    ("kitty.json", include_str!("../../../specs/kitty.json")),
    ("klist.json", include_str!("../../../specs/klist.json")),
    ("knex.json", include_str!("../../../specs/knex.json")),
    ("kool.json", include_str!("../../../specs/kool.json")),
    ("kotlinc.json", include_str!("../../../specs/kotlinc.json")),
    (
        "kubecolor.json",
        include_str!("../../../specs/kubecolor.json"),
    ),
    ("kubectl.json", include_str!("../../../specs/kubectl.json")),
    ("kubectx.json", include_str!("../../../specs/kubectx.json")),
    ("kubens.json", include_str!("../../../specs/kubens.json")),
    ("laravel.json", include_str!("../../../specs/laravel.json")),
    (
        "launchctl.json",
        include_str!("../../../specs/launchctl.json"),
    ),
    ("ldd.json", include_str!("../../../specs/ldd.json")),
    ("leaf.json", include_str!("../../../specs/leaf.json")),
    ("lerna.json", include_str!("../../../specs/lerna.json")),
    ("less.json", include_str!("../../../specs/less.json")),
    ("lima.json", include_str!("../../../specs/lima.json")),
    ("limactl.json", include_str!("../../../specs/limactl.json")),
    ("ln.json", include_str!("../../../specs/ln.json")),
    ("locust.json", include_str!("../../../specs/locust.json")),
    ("login.json", include_str!("../../../specs/login.json")),
    ("lp.json", include_str!("../../../specs/lp.json")),
    ("lpass.json", include_str!("../../../specs/lpass.json")),
    ("ls.json", include_str!("../../../specs/ls.json")),
    ("lsblk.json", include_str!("../../../specs/lsblk.json")),
    ("lsd.json", include_str!("../../../specs/lsd.json")),
    ("lsof.json", include_str!("../../../specs/lsof.json")),
    ("luz.json", include_str!("../../../specs/luz.json")),
    ("lvim.json", include_str!("../../../specs/lvim.json")),
    ("m.json", include_str!("../../../specs/m.json")),
    ("mackup.json", include_str!("../../../specs/mackup.json")),
    ("magento.json", include_str!("../../../specs/magento.json")),
    ("maigret.json", include_str!("../../../specs/maigret.json")),
    ("mailsy.json", include_str!("../../../specs/mailsy.json")),
    ("make.json", include_str!("../../../specs/make.json")),
    ("mamba.json", include_str!("../../../specs/mamba.json")),
    ("man.json", include_str!("../../../specs/man.json")),
    ("mas.json", include_str!("../../../specs/mas.json")),
    ("mask.json", include_str!("../../../specs/mask.json")),
    ("mdfind.json", include_str!("../../../specs/mdfind.json")),
    ("mdls.json", include_str!("../../../specs/mdls.json")),
    ("meroxa.json", include_str!("../../../specs/meroxa.json")),
    ("meteor.json", include_str!("../../../specs/meteor.json")),
    ("mgnl.json", include_str!("../../../specs/mgnl.json")),
    ("micro.json", include_str!("../../../specs/micro.json")),
    (
        "mikro-orm.json",
        include_str!("../../../specs/mikro-orm.json"),
    ),
    ("minectl.json", include_str!("../../../specs/minectl.json")),
    (
        "minikube.json",
        include_str!("../../../specs/minikube.json"),
    ),
    ("mix.json", include_str!("../../../specs/mix.json")),
    ("mkdir.json", include_str!("../../../specs/mkdir.json")),
    ("mkdocs.json", include_str!("../../../specs/mkdocs.json")),
    ("mkfifo.json", include_str!("../../../specs/mkfifo.json")),
    (
        "mkinitcpio.json",
        include_str!("../../../specs/mkinitcpio.json"),
    ),
    ("mknod.json", include_str!("../../../specs/mknod.json")),
    ("mob.json", include_str!("../../../specs/mob.json")),
    (
        "molecule.json",
        include_str!("../../../specs/molecule.json"),
    ),
    (
        "mongoimport.json",
        include_str!("../../../specs/mongoimport.json"),
    ),
    ("mongosh.json", include_str!("../../../specs/mongosh.json")),
    ("more.json", include_str!("../../../specs/more.json")),
    ("mosh.json", include_str!("../../../specs/mosh.json")),
    ("mount.json", include_str!("../../../specs/mount.json")),
    (
        "multipass.json",
        include_str!("../../../specs/multipass.json"),
    ),
    ("mv.json", include_str!("../../../specs/mv.json")),
    ("mvn.json", include_str!("../../../specs/mvn.json")),
    ("mypy.json", include_str!("../../../specs/mypy.json")),
    ("mysql.json", include_str!("../../../specs/mysql.json")),
    ("n.json", include_str!("../../../specs/n.json")),
    ("nano.json", include_str!("../../../specs/nano.json")),
    (
        "nativescript.json",
        include_str!("../../../specs/nativescript.json"),
    ),
    ("nc.json", include_str!("../../../specs/nc.json")),
    ("ncal.json", include_str!("../../../specs/ncal.json")),
    ("ncu.json", include_str!("../../../specs/ncu.json")),
    (
        "neofetch.json",
        include_str!("../../../specs/neofetch.json"),
    ),
    ("nest.json", include_str!("../../../specs/nest.json")),
    ("netlify.json", include_str!("../../../specs/netlify.json")),
    (
        "networkQuality.json",
        include_str!("../../../specs/networkQuality.json"),
    ),
    (
        "networksetup.json",
        include_str!("../../../specs/networksetup.json"),
    ),
    ("newman.json", include_str!("../../../specs/newman.json")),
    ("next.json", include_str!("../../../specs/next.json")),
    (
        "nextflow.json",
        include_str!("../../../specs/nextflow.json"),
    ),
    ("ng.json", include_str!("../../../specs/ng.json")),
    ("nginx.json", include_str!("../../../specs/nginx.json")),
    ("ngrok.json", include_str!("../../../specs/ngrok.json")),
    ("nhost.json", include_str!("../../../specs/nhost.json")),
    ("ni.json", include_str!("../../../specs/ni.json")),
    ("nl.json", include_str!("../../../specs/nl.json")),
    ("nmap.json", include_str!("../../../specs/nmap.json")),
    (
        "nocorrect.json",
        include_str!("../../../specs/nocorrect.json"),
    ),
    ("node.json", include_str!("../../../specs/node.json")),
    ("noglob.json", include_str!("../../../specs/noglob.json")),
    ("np.json", include_str!("../../../specs/np.json")),
    ("npm.json", include_str!("../../../specs/npm.json")),
    ("npx.json", include_str!("../../../specs/npx.json")),
    ("nr.json", include_str!("../../../specs/nr.json")),
    ("nrm.json", include_str!("../../../specs/nrm.json")),
    ("ns.json", include_str!("../../../specs/ns.json")),
    ("nu.json", include_str!("../../../specs/nu.json")),
    ("nuxi.json", include_str!("../../../specs/nuxi.json")),
    ("nuxt.json", include_str!("../../../specs/nuxt.json")),
    ("nvim.json", include_str!("../../../specs/nvim.json")),
    ("nvm.json", include_str!("../../../specs/nvm.json")),
    ("nx.json", include_str!("../../../specs/nx.json")),
    ("nylas.json", include_str!("../../../specs/nylas.json")),
    ("oci.json", include_str!("../../../specs/oci.json")),
    ("od.json", include_str!("../../../specs/od.json")),
    (
        "oh-my-posh.json",
        include_str!("../../../specs/oh-my-posh.json"),
    ),
    ("okta.json", include_str!("../../../specs/okta.json")),
    ("okteto.json", include_str!("../../../specs/okteto.json")),
    ("ollama.json", include_str!("../../../specs/ollama.json")),
    ("omz.json", include_str!("../../../specs/omz.json")),
    (
        "onboardbase.json",
        include_str!("../../../specs/onboardbase.json"),
    ),
    ("op.json", include_str!("../../../specs/op.json")),
    ("opa.json", include_str!("../../../specs/opa.json")),
    ("open.json", include_str!("../../../specs/open.json")),
    (
        "osascript.json",
        include_str!("../../../specs/osascript.json"),
    ),
    (
        "osqueryi.json",
        include_str!("../../../specs/osqueryi.json"),
    ),
    ("oxlint.json", include_str!("../../../specs/oxlint.json")),
    ("pac.json", include_str!("../../../specs/pac.json")),
    ("pageres.json", include_str!("../../../specs/pageres.json")),
    (
        "palera1n.json",
        include_str!("../../../specs/palera1n.json"),
    ),
    ("pandoc.json", include_str!("../../../specs/pandoc.json")),
    ("paper.json", include_str!("../../../specs/paper.json")),
    ("pass.json", include_str!("../../../specs/pass.json")),
    ("passwd.json", include_str!("../../../specs/passwd.json")),
    ("paste.json", include_str!("../../../specs/paste.json")),
    ("pathchk.json", include_str!("../../../specs/pathchk.json")),
    (
        "pdfunite.json",
        include_str!("../../../specs/pdfunite.json"),
    ),
    ("pg_dump.json", include_str!("../../../specs/pg_dump.json")),
    ("pgcli.json", include_str!("../../../specs/pgcli.json")),
    ("php.json", include_str!("../../../specs/php.json")),
    (
        "phpstorm.json",
        include_str!("../../../specs/phpstorm.json"),
    ),
    (
        "phpunit-watcher.json",
        include_str!("../../../specs/phpunit-watcher.json"),
    ),
    ("phpunit.json", include_str!("../../../specs/phpunit.json")),
    ("pijul.json", include_str!("../../../specs/pijul.json")),
    ("ping.json", include_str!("../../../specs/ping.json")),
    ("pip.json", include_str!("../../../specs/pip.json")),
    ("pip3.json", include_str!("../../../specs/pip3.json")),
    ("pipenv.json", include_str!("../../../specs/pipenv.json")),
    ("pipx.json", include_str!("../../../specs/pipx.json")),
    (
        "pkg-config.json",
        include_str!("../../../specs/pkg-config.json"),
    ),
    ("pkgutil.json", include_str!("../../../specs/pkgutil.json")),
    ("pkill.json", include_str!("../../../specs/pkill.json")),
    ("planter.json", include_str!("../../../specs/planter.json")),
    (
        "playwright.json",
        include_str!("../../../specs/playwright.json"),
    ),
    ("plutil.json", include_str!("../../../specs/plutil.json")),
    ("pm2.json", include_str!("../../../specs/pm2.json")),
    ("pmset.json", include_str!("../../../specs/pmset.json")),
    ("pnpm.json", include_str!("../../../specs/pnpm.json")),
    ("pnpx.json", include_str!("../../../specs/pnpx.json")),
    (
        "pocketbase.json",
        include_str!("../../../specs/pocketbase.json"),
    ),
    ("pod.json", include_str!("../../../specs/pod.json")),
    ("podman.json", include_str!("../../../specs/podman.json")),
    ("poetry.json", include_str!("../../../specs/poetry.json")),
    (
        "pre-commit.json",
        include_str!("../../../specs/pre-commit.json"),
    ),
    ("premake.json", include_str!("../../../specs/premake.json")),
    ("preset.json", include_str!("../../../specs/preset.json")),
    (
        "prettier.json",
        include_str!("../../../specs/prettier.json"),
    ),
    ("prisma.json", include_str!("../../../specs/prisma.json")),
    ("pro.json", include_str!("../../../specs/pro.json")),
    (
        "progressline.json",
        include_str!("../../../specs/progressline.json"),
    ),
    ("projj.json", include_str!("../../../specs/projj.json")),
    ("pry.json", include_str!("../../../specs/pry.json")),
    ("ps.json", include_str!("../../../specs/ps.json")),
    ("pscale.json", include_str!("../../../specs/pscale.json")),
    ("psql.json", include_str!("../../../specs/psql.json")),
    ("publish.json", include_str!("../../../specs/publish.json")),
    ("pulumi.json", include_str!("../../../specs/pulumi.json")),
    ("pushd.json", include_str!("../../../specs/pushd.json")),
    ("pwd.json", include_str!("../../../specs/pwd.json")),
    ("pycharm.json", include_str!("../../../specs/pycharm.json")),
    ("pyenv.json", include_str!("../../../specs/pyenv.json")),
    ("pytest.json", include_str!("../../../specs/pytest.json")),
    ("python.json", include_str!("../../../specs/python.json")),
    ("python3.json", include_str!("../../../specs/python3.json")),
    ("q.json", include_str!("../../../specs/q.json")),
    ("qodana.json", include_str!("../../../specs/qodana.json")),
    ("quasar.json", include_str!("../../../specs/quasar.json")),
    (
        "quickmail.json",
        include_str!("../../../specs/quickmail.json"),
    ),
    ("r.json", include_str!("../../../specs/r.json")),
    ("rails.json", include_str!("../../../specs/rails.json")),
    ("railway.json", include_str!("../../../specs/railway.json")),
    ("rake.json", include_str!("../../../specs/rake.json")),
    ("rancher.json", include_str!("../../../specs/rancher.json")),
    ("rbenv.json", include_str!("../../../specs/rbenv.json")),
    ("rclone.json", include_str!("../../../specs/rclone.json")),
    (
        "react-native.json",
        include_str!("../../../specs/react-native.json"),
    ),
    (
        "readlink.json",
        include_str!("../../../specs/readlink.json"),
    ),
    ("redwood.json", include_str!("../../../specs/redwood.json")),
    ("remix.json", include_str!("../../../specs/remix.json")),
    (
        "remotion.json",
        include_str!("../../../specs/remotion.json"),
    ),
    ("repeat.json", include_str!("../../../specs/repeat.json")),
    ("rg.json", include_str!("../../../specs/rg.json")),
    ("rich.json", include_str!("../../../specs/rich.json")),
    ("rm.json", include_str!("../../../specs/rm.json")),
    ("rmdir.json", include_str!("../../../specs/rmdir.json")),
    ("robot.json", include_str!("../../../specs/robot.json")),
    ("rojo.json", include_str!("../../../specs/rojo.json")),
    ("rollup.json", include_str!("../../../specs/rollup.json")),
    ("rome.json", include_str!("../../../specs/rome.json")),
    ("rscript.json", include_str!("../../../specs/rscript.json")),
    ("rsync.json", include_str!("../../../specs/rsync.json")),
    ("rubocop.json", include_str!("../../../specs/rubocop.json")),
    ("ruby.json", include_str!("../../../specs/ruby.json")),
    (
        "rubymine.json",
        include_str!("../../../specs/rubymine.json"),
    ),
    ("ruff.json", include_str!("../../../specs/ruff.json")),
    ("rugby.json", include_str!("../../../specs/rugby.json")),
    ("rush.json", include_str!("../../../specs/rush.json")),
    ("rushx.json", include_str!("../../../specs/rushx.json")),
    ("rustc.json", include_str!("../../../specs/rustc.json")),
    (
        "rustrover.json",
        include_str!("../../../specs/rustrover.json"),
    ),
    ("rustup.json", include_str!("../../../specs/rustup.json")),
    ("rvm.json", include_str!("../../../specs/rvm.json")),
    ("sake.json", include_str!("../../../specs/sake.json")),
    ("sam.json", include_str!("../../../specs/sam.json")),
    ("sanity.json", include_str!("../../../specs/sanity.json")),
    (
        "sapphire.json",
        include_str!("../../../specs/sapphire.json"),
    ),
    ("scarb.json", include_str!("../../../specs/scarb.json")),
    ("scc.json", include_str!("../../../specs/scc.json")),
    ("scp.json", include_str!("../../../specs/scp.json")),
    ("screen.json", include_str!("../../../specs/screen.json")),
    ("sed.json", include_str!("../../../specs/sed.json")),
    ("seq.json", include_str!("../../../specs/seq.json")),
    (
        "sequelize.json",
        include_str!("../../../specs/sequelize.json"),
    ),
    ("serve.json", include_str!("../../../specs/serve.json")),
    (
        "serverless.json",
        include_str!("../../../specs/serverless.json"),
    ),
    ("sftp.json", include_str!("../../../specs/sftp.json")),
    ("sha1sum.json", include_str!("../../../specs/sha1sum.json")),
    (
        "shadcn-ui.json",
        include_str!("../../../specs/shadcn-ui.json"),
    ),
    ("shasum.json", include_str!("../../../specs/shasum.json")),
    (
        "shell-config.json",
        include_str!("../../../specs/shell-config.json"),
    ),
    ("shelve.json", include_str!("../../../specs/shelve.json")),
    (
        "shortcuts.json",
        include_str!("../../../specs/shortcuts.json"),
    ),
    ("shred.json", include_str!("../../../specs/shred.json")),
    ("sidekiq.json", include_str!("../../../specs/sidekiq.json")),
    ("simctl.json", include_str!("../../../specs/simctl.json")),
    ("sips.json", include_str!("../../../specs/sips.json")),
    ("sl.json", include_str!("../../../specs/sl.json")),
    ("sls.json", include_str!("../../../specs/sls.json")),
    ("snaplet.json", include_str!("../../../specs/snaplet.json")),
    (
        "softwareupdate.json",
        include_str!("../../../specs/softwareupdate.json"),
    ),
    ("sort.json", include_str!("../../../specs/sort.json")),
    ("source.json", include_str!("../../../specs/source.json")),
    ("space.json", include_str!("../../../specs/space.json")),
    (
        "speedtest-cli.json",
        include_str!("../../../specs/speedtest-cli.json"),
    ),
    (
        "speedtest.json",
        include_str!("../../../specs/speedtest.json"),
    ),
    ("splash.json", include_str!("../../../specs/splash.json")),
    ("split.json", include_str!("../../../specs/split.json")),
    ("spotify.json", include_str!("../../../specs/spotify.json")),
    ("spring.json", include_str!("../../../specs/spring.json")),
    (
        "sqlfluff.json",
        include_str!("../../../specs/sqlfluff.json"),
    ),
    ("sqlite3.json", include_str!("../../../specs/sqlite3.json")),
    ("sqlmesh.json", include_str!("../../../specs/sqlmesh.json")),
    ("src.json", include_str!("../../../specs/src.json")),
    (
        "ssh-keygen.json",
        include_str!("../../../specs/ssh-keygen.json"),
    ),
    ("ssh.json", include_str!("../../../specs/ssh.json")),
    ("st2.json", include_str!("../../../specs/st2.json")),
    ("sta.json", include_str!("../../../specs/sta.json")),
    ("stack.json", include_str!("../../../specs/stack.json")),
    ("starkli.json", include_str!("../../../specs/starkli.json")),
    (
        "start-storybook.json",
        include_str!("../../../specs/start-storybook.json"),
    ),
    ("stat.json", include_str!("../../../specs/stat.json")),
    (
        "steadybit.json",
        include_str!("../../../specs/steadybit.json"),
    ),
    ("stencil.json", include_str!("../../../specs/stencil.json")),
    ("stepzen.json", include_str!("../../../specs/stepzen.json")),
    ("stow.json", include_str!("../../../specs/stow.json")),
    (
        "streamlit.json",
        include_str!("../../../specs/streamlit.json"),
    ),
    ("stripe.json", include_str!("../../../specs/stripe.json")),
    ("su.json", include_str!("../../../specs/su.json")),
    ("subl.json", include_str!("../../../specs/subl.json")),
    ("sudo.json", include_str!("../../../specs/sudo.json")),
    (
        "suitecloud.json",
        include_str!("../../../specs/suitecloud.json"),
    ),
    (
        "supabase.json",
        include_str!("../../../specs/supabase.json"),
    ),
    ("surreal.json", include_str!("../../../specs/surreal.json")),
    ("svn.json", include_str!("../../../specs/svn.json")),
    ("svokit.json", include_str!("../../../specs/svokit.json")),
    (
        "svtplay-dl.json",
        include_str!("../../../specs/svtplay-dl.json"),
    ),
    ("sw_vers.json", include_str!("../../../specs/sw_vers.json")),
    (
        "swagger-typescript-api.json",
        include_str!("../../../specs/swagger-typescript-api.json"),
    ),
    ("swc.json", include_str!("../../../specs/swc.json")),
    ("swift.json", include_str!("../../../specs/swift.json")),
    ("symfony.json", include_str!("../../../specs/symfony.json")),
    ("sysctl.json", include_str!("../../../specs/sysctl.json")),
    (
        "systemctl.json",
        include_str!("../../../specs/systemctl.json"),
    ),
    ("tac.json", include_str!("../../../specs/tac.json")),
    ("tail.json", include_str!("../../../specs/tail.json")),
    (
        "tailcall.json",
        include_str!("../../../specs/tailcall.json"),
    ),
    (
        "tailscale.json",
        include_str!("../../../specs/tailscale.json"),
    ),
    (
        "tailwindcss.json",
        include_str!("../../../specs/tailwindcss.json"),
    ),
    ("tangram.json", include_str!("../../../specs/tangram.json")),
    ("taplo.json", include_str!("../../../specs/taplo.json")),
    ("tar.json", include_str!("../../../specs/tar.json")),
    ("task.json", include_str!("../../../specs/task.json")),
    ("tb.json", include_str!("../../../specs/tb.json")),
    ("tccutil.json", include_str!("../../../specs/tccutil.json")),
    ("tee.json", include_str!("../../../specs/tee.json")),
    (
        "terraform.json",
        include_str!("../../../specs/terraform.json"),
    ),
    (
        "terragrunt.json",
        include_str!("../../../specs/terragrunt.json"),
    ),
    ("tfenv.json", include_str!("../../../specs/tfenv.json")),
    ("tfsec.json", include_str!("../../../specs/tfsec.json")),
    ("then.json", include_str!("../../../specs/then.json")),
    ("time.json", include_str!("../../../specs/time.json")),
    ("tkn.json", include_str!("../../../specs/tkn.json")),
    ("tldr.json", include_str!("../../../specs/tldr.json")),
    ("tmutil.json", include_str!("../../../specs/tmutil.json")),
    ("tmux.json", include_str!("../../../specs/tmux.json")),
    (
        "tmuxinator.json",
        include_str!("../../../specs/tmuxinator.json"),
    ),
    ("tns.json", include_str!("../../../specs/tns.json")),
    ("tokei.json", include_str!("../../../specs/tokei.json")),
    ("top.json", include_str!("../../../specs/top.json")),
    ("touch.json", include_str!("../../../specs/touch.json")),
    ("tr.json", include_str!("../../../specs/tr.json")),
    (
        "traceroute.json",
        include_str!("../../../specs/traceroute.json"),
    ),
    ("trap.json", include_str!("../../../specs/trap.json")),
    ("trash.json", include_str!("../../../specs/trash.json")),
    ("tree.json", include_str!("../../../specs/tree.json")),
    ("trex.json", include_str!("../../../specs/trex.json")),
    ("trivy.json", include_str!("../../../specs/trivy.json")),
    ("truffle.json", include_str!("../../../specs/truffle.json")),
    (
        "truncate.json",
        include_str!("../../../specs/truncate.json"),
    ),
    ("trunk.json", include_str!("../../../specs/trunk.json")),
    ("ts-node.json", include_str!("../../../specs/ts-node.json")),
    ("tsc.json", include_str!("../../../specs/tsc.json")),
    ("tsh.json", include_str!("../../../specs/tsh.json")),
    ("tsuru.json", include_str!("../../../specs/tsuru.json")),
    ("tsx.json", include_str!("../../../specs/tsx.json")),
    ("tuist.json", include_str!("../../../specs/tuist.json")),
    ("turbo.json", include_str!("../../../specs/turbo.json")),
    ("twiggy.json", include_str!("../../../specs/twiggy.json")),
    ("typeorm.json", include_str!("../../../specs/typeorm.json")),
    ("typos.json", include_str!("../../../specs/typos.json")),
    ("typst.json", include_str!("../../../specs/typst.json")),
    ("ua.json", include_str!("../../../specs/ua.json")),
    (
        "ubuntu-advantage.json",
        include_str!("../../../specs/ubuntu-advantage.json"),
    ),
    ("uname.json", include_str!("../../../specs/uname.json")),
    ("uniq.json", include_str!("../../../specs/uniq.json")),
    (
        "unix2dos.json",
        include_str!("../../../specs/unix2dos.json"),
    ),
    ("unset.json", include_str!("../../../specs/unset.json")),
    ("until.json", include_str!("../../../specs/until.json")),
    ("unzip.json", include_str!("../../../specs/unzip.json")),
    ("uv.json", include_str!("../../../specs/uv.json")),
    ("v.json", include_str!("../../../specs/v.json")),
    ("vale.json", include_str!("../../../specs/vale.json")),
    ("valet.json", include_str!("../../../specs/valet.json")),
    ("vapor.json", include_str!("../../../specs/vapor.json")),
    ("vault.json", include_str!("../../../specs/vault.json")),
    ("vela.json", include_str!("../../../specs/vela.json")),
    ("vercel.json", include_str!("../../../specs/vercel.json")),
    ("vi.json", include_str!("../../../specs/vi.json")),
    ("vim.json", include_str!("../../../specs/vim.json")),
    ("vimr.json", include_str!("../../../specs/vimr.json")),
    ("visudo.json", include_str!("../../../specs/visudo.json")),
    ("vite.json", include_str!("../../../specs/vite.json")),
    ("volta.json", include_str!("../../../specs/volta.json")),
    ("vr.json", include_str!("../../../specs/vr.json")),
    ("vsce.json", include_str!("../../../specs/vsce.json")),
    ("vtex.json", include_str!("../../../specs/vtex.json")),
    ("vue.json", include_str!("../../../specs/vue.json")),
    (
        "vultr-cli.json",
        include_str!("../../../specs/vultr-cli.json"),
    ),
    ("w.json", include_str!("../../../specs/w.json")),
    (
        "wasm-bindgen.json",
        include_str!("../../../specs/wasm-bindgen.json"),
    ),
    (
        "wasm-pack.json",
        include_str!("../../../specs/wasm-pack.json"),
    ),
    (
        "watchman.json",
        include_str!("../../../specs/watchman.json"),
    ),
    ("watson.json", include_str!("../../../specs/watson.json")),
    ("wc.json", include_str!("../../../specs/wc.json")),
    ("wd.json", include_str!("../../../specs/wd.json")),
    ("webpack.json", include_str!("../../../specs/webpack.json")),
    (
        "webstorm.json",
        include_str!("../../../specs/webstorm.json"),
    ),
    ("wezterm.json", include_str!("../../../specs/wezterm.json")),
    ("wget.json", include_str!("../../../specs/wget.json")),
    ("whence.json", include_str!("../../../specs/whence.json")),
    ("where.json", include_str!("../../../specs/where.json")),
    ("whereis.json", include_str!("../../../specs/whereis.json")),
    ("which.json", include_str!("../../../specs/which.json")),
    ("while.json", include_str!("../../../specs/while.json")),
    ("who.json", include_str!("../../../specs/who.json")),
    ("whois.json", include_str!("../../../specs/whois.json")),
    (
        "wifi-password.json",
        include_str!("../../../specs/wifi-password.json"),
    ),
    ("wing.json", include_str!("../../../specs/wing.json")),
    ("wp.json", include_str!("../../../specs/wp.json")),
    (
        "wrangler.json",
        include_str!("../../../specs/wrangler.json"),
    ),
    ("wrk.json", include_str!("../../../specs/wrk.json")),
    ("wscat.json", include_str!("../../../specs/wscat.json")),
    ("xargs.json", include_str!("../../../specs/xargs.json")),
    ("xc.json", include_str!("../../../specs/xc.json")),
    (
        "xcode-select.json",
        include_str!("../../../specs/xcode-select.json"),
    ),
    (
        "xcodebuild.json",
        include_str!("../../../specs/xcodebuild.json"),
    ),
    (
        "xcodeproj.json",
        include_str!("../../../specs/xcodeproj.json"),
    ),
    ("xcodes.json", include_str!("../../../specs/xcodes.json")),
    ("xcrun.json", include_str!("../../../specs/xcrun.json")),
    (
        "xdg-mime.json",
        include_str!("../../../specs/xdg-mime.json"),
    ),
    (
        "xdg-open.json",
        include_str!("../../../specs/xdg-open.json"),
    ),
    ("xed.json", include_str!("../../../specs/xed.json")),
    ("xxd.json", include_str!("../../../specs/xxd.json")),
    ("yalc.json", include_str!("../../../specs/yalc.json")),
    ("yank.json", include_str!("../../../specs/yank.json")),
    ("yarn.json", include_str!("../../../specs/yarn.json")),
    ("ykman.json", include_str!("../../../specs/ykman.json")),
    ("yo.json", include_str!("../../../specs/yo.json")),
    ("yomo.json", include_str!("../../../specs/yomo.json")),
    (
        "youtube-dl.json",
        include_str!("../../../specs/youtube-dl.json"),
    ),
    ("z.json", include_str!("../../../specs/z.json")),
    ("zapier.json", include_str!("../../../specs/zapier.json")),
    ("zed.json", include_str!("../../../specs/zed.json")),
    ("zellij.json", include_str!("../../../specs/zellij.json")),
    ("zig.json", include_str!("../../../specs/zig.json")),
    ("zip.json", include_str!("../../../specs/zip.json")),
    (
        "zipcloak.json",
        include_str!("../../../specs/zipcloak.json"),
    ),
    ("zoxide.json", include_str!("../../../specs/zoxide.json")),
];

const DEFAULT_CONFIG_TOML: &str = "\
# Ghost Complete configuration
# Uncomment and edit values to customize. All values shown are defaults.

# [trigger]
# auto_chars = [' ', '/', '-', '.']
# delay_ms = 150

# [popup]
# max_visible = 10
# min_width = 20
# max_width = 60

# [suggest]
# max_results = 50
# max_history_results = 5
# max_history_entries = 10000

# [suggest.providers]
# commands = true
# filesystem = true
# specs = true
# git = true

# [keybindings]
# accept = \"tab\"
# accept_and_enter = \"enter\"
# dismiss = \"escape\"
# navigate_up = \"arrow_up\"
# navigate_down = \"arrow_down\"
# trigger = \"ctrl+/\"

[theme]
# preset = \"dark\"
# selected = \"reverse\"
# description = \"dim\"
# match_highlight = \"bold\"
# item_text = \"\"
# scrollbar = \"dim\"

# [experimental]
# multi_terminal = false  # Set to true to enable iTerm2 and Terminal.app support
";

const INIT_BEGIN: &str = "# >>> ghost-complete initialize >>>";
const INIT_END: &str = "# <<< ghost-complete initialize <<<";
const SHELL_BEGIN: &str = "# >>> ghost-complete shell integration >>>";
const SHELL_END: &str = "# <<< ghost-complete shell integration <<<";
const MANAGED_WARNING: &str =
    "# !! Contents within this block are managed by 'ghost-complete install' !!";

/// Shell variable holding the config file path (set once in the init block).
const CONFIG_VAR: &str = "_gc_config";
/// Condition: config file exists AND contains multi_terminal = true.
/// File-existence check is silent; grep errors on an existing file are visible.
const MULTI_TERMINAL_CHECK: &str = "[[ -f \"$_gc_config\" ]] && grep -qE '^[[:space:]]*multi_terminal[[:space:]]*=[[:space:]]*true' \"$_gc_config\"";

fn init_block() -> String {
    format!(
        "{INIT_BEGIN}\n\
         {MANAGED_WARNING}\n\
         {CONFIG_VAR}=\"$HOME/.config/ghost-complete/config.toml\"\n\
         case \"$TERM_PROGRAM\" in\n  \
           ghostty)\n    \
             if [[ -z \"$GHOST_COMPLETE_ACTIVE\" ]]; then\n      \
               export GHOST_COMPLETE_ACTIVE=1\n      \
               exec ghost-complete\n    \
             fi\n    \
             ;;\n  \
           iTerm.app|Apple_Terminal)\n    \
             if [[ -z \"$GHOST_COMPLETE_ACTIVE\" ]] && {MULTI_TERMINAL_CHECK}; then\n      \
               export GHOST_COMPLETE_ACTIVE=1\n      \
               exec ghost-complete\n    \
             fi\n    \
             ;;\n\
         esac\n\
         if [[ -n \"$TMUX\" && -z \"$GHOST_COMPLETE_ACTIVE\" && \\\n    \
           \"$(ps -o comm= -p $PPID 2>/dev/null)\" != \"ghost-complete\" ]]; then\n  \
           if [[ -n \"$GHOSTTY_RESOURCES_DIR\" ]]; then\n    \
             export GHOST_COMPLETE_ACTIVE=1\n    \
             exec ghost-complete\n  \
           elif [[ -n \"$ITERM_SESSION_ID\" ]] && {MULTI_TERMINAL_CHECK}; then\n    \
             export GHOST_COMPLETE_ACTIVE=1\n    \
             exec ghost-complete\n  \
           fi\n\
         fi\n\
         unset {CONFIG_VAR}\n\
         {INIT_END}"
    )
}

fn shell_integration_block(script_path: &Path) -> String {
    format!(
        "{SHELL_BEGIN}\n\
         {MANAGED_WARNING}\n\
         source \"{}\"\n\
         {SHELL_END}",
        script_path.display()
    )
}

/// Strips a managed block delimited by `begin`..`end` markers from `content`.
/// Returns `(new_content, was_found)`.
fn remove_block(content: &str, begin: &str, end: &str) -> (String, bool) {
    let Some(start_idx) = content.find(begin) else {
        return (content.to_string(), false);
    };
    let Some(end_match) = content[start_idx..].find(end) else {
        return (content.to_string(), false);
    };
    let end_idx = start_idx + end_match + end.len();

    let mut result = String::with_capacity(content.len());
    result.push_str(&content[..start_idx]);
    // Skip trailing newline after end marker if present
    let after = if content[end_idx..].starts_with('\n') {
        &content[end_idx + 1..]
    } else {
        &content[end_idx..]
    };
    result.push_str(after);

    (result, true)
}

fn copy_specs(config_dir: &Path) -> Result<()> {
    let dest = config_dir.join("specs");
    fs::create_dir_all(&dest).with_context(|| format!("failed to create {}", dest.display()))?;

    let mut count = 0;
    for (name, contents) in EMBEDDED_SPECS {
        let dest_file = dest.join(name);
        fs::write(&dest_file, contents)
            .with_context(|| format!("failed to write spec: {}", dest_file.display()))?;
        count += 1;
    }
    println!("  Installed {count} completion specs to {}", dest.display());
    Ok(())
}

fn print_shell_blocks(script_path: &Path) {
    let init = init_block();
    let shell = shell_integration_block(script_path);
    let indented_init = init.replace('\n', "\n    ");
    let indented_shell = shell.replace('\n', "\n    ");

    println!(
        "  \x1b[36m\u{2139}\x1b[0m  Add the following \x1b[1mNEAR THE TOP\x1b[0m of your shell config:\n"
    );
    println!("    \x1b[36m{indented_init}\x1b[0m\n");
    println!(
        "  \x1b[36m\u{2139}\x1b[0m  Add the following \x1b[1mNEAR THE BOTTOM\x1b[0m of your shell config:\n"
    );
    println!("    \x1b[36m{indented_shell}\x1b[0m\n");
}

fn install_to(zshrc_path: &Path, config_dir: &Path, dry_run: bool) -> Result<()> {
    // 1. Write zsh shell integration script
    let shell_dir = config_dir.join("shell");
    let script_path = shell_dir.join("ghost-complete.zsh");

    if dry_run {
        println!("  Would write zsh integration to {}", script_path.display());
        println!(
            "  Would install {} completion specs to {}",
            EMBEDDED_SPECS.len(),
            config_dir.join("specs").display()
        );
        let config_path = config_dir.join("config.toml");
        if !config_path.exists() {
            println!("  Would write default config to {}", config_path.display());
        } else {
            println!("  Config already exists at {}", config_path.display());
        }
        println!("  Would update {}\n", zshrc_path.display());
        println!("  \x1b[36m\u{2139}\x1b[0m  The following would be added to your shell config:\n");
        print_shell_blocks(&script_path);
        return Ok(());
    }

    fs::create_dir_all(&shell_dir)
        .with_context(|| format!("failed to create {}", shell_dir.display()))?;

    fs::write(&script_path, ZSH_INTEGRATION)
        .with_context(|| format!("failed to write {}", script_path.display()))?;
    println!("  Wrote zsh integration to {}", script_path.display());

    // 1b. Copy completion specs
    copy_specs(config_dir)?;

    // 1c. Write default config.toml if one doesn't exist (never clobber)
    let config_path = config_dir.join("config.toml");
    if !config_path.exists() {
        fs::write(&config_path, DEFAULT_CONFIG_TOML)
            .with_context(|| format!("failed to write {}", config_path.display()))?;
        println!("  Wrote default config to {}", config_path.display());
    } else {
        println!("  Config already exists at {}", config_path.display());
    }

    // 2. Read existing .zshrc (or empty)
    let existing = if zshrc_path.exists() {
        fs::read_to_string(zshrc_path)
            .with_context(|| format!("failed to read {}", zshrc_path.display()))?
    } else {
        String::new()
    };

    // 3. Backup
    if zshrc_path.exists() {
        let backup = zshrc_path.with_extension("backup.ghost-complete");
        fs::copy(zshrc_path, &backup)
            .with_context(|| format!("failed to backup to {}", backup.display()))?;
        println!("  Backed up .zshrc to {}", backup.display());
    }

    // 4. Strip existing managed blocks (idempotent)
    let (content, _) = remove_block(&existing, INIT_BEGIN, INIT_END);
    let (content, _) = remove_block(&content, SHELL_BEGIN, SHELL_END);

    // 5. Prepend init block, append shell integration block
    let content = content.trim().to_string();
    let mut new_zshrc = String::new();
    new_zshrc.push_str(&init_block());
    new_zshrc.push('\n');
    if !content.is_empty() {
        new_zshrc.push_str(&content);
        new_zshrc.push('\n');
    }
    new_zshrc.push_str(&shell_integration_block(&script_path));
    new_zshrc.push('\n');

    // 6. Write .zshrc — graceful fallback if permission denied (e.g. nix-managed)
    match fs::write(zshrc_path, &new_zshrc) {
        Ok(()) => {
            println!("  Updated {}", zshrc_path.display());
            println!("\nghost-complete installed successfully!");
            println!("Restart your shell or run: source ~/.zshrc");
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            println!(
                "\n  \x1b[33m\u{26a0}  Could not write to {} (permission denied)\x1b[0m\n",
                zshrc_path.display()
            );
            print_shell_blocks(&script_path);
            println!(
                "  \x1b[32m\u{2713}\x1b[0m  Installation complete (manual shell configuration required)."
            );
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "failed to write {}: {}",
                zshrc_path.display(),
                e
            ));
        }
    }
    Ok(())
}

fn uninstall_from(zshrc_path: &Path, config_dir: &Path) -> Result<()> {
    // 1. Strip managed blocks from .zshrc
    if zshrc_path.exists() {
        let content = fs::read_to_string(zshrc_path)
            .with_context(|| format!("failed to read {}", zshrc_path.display()))?;

        let (content, found_init) = remove_block(&content, INIT_BEGIN, INIT_END);
        let (content, found_shell) = remove_block(&content, SHELL_BEGIN, SHELL_END);

        if found_init || found_shell {
            fs::write(zshrc_path, &content)
                .with_context(|| format!("failed to write {}", zshrc_path.display()))?;
            println!("  Removed managed blocks from {}", zshrc_path.display());
        } else {
            println!(
                "  No ghost-complete blocks found in {}",
                zshrc_path.display()
            );
        }
    } else {
        println!("  {} does not exist, nothing to do", zshrc_path.display());
    }

    // 2. Remove shell integration scripts
    for name in &[
        "ghost-complete.zsh",
        "ghost-complete.bash",
        "ghost-complete.fish",
    ] {
        let script_path = config_dir.join("shell").join(name);
        if script_path.exists() {
            fs::remove_file(&script_path)
                .with_context(|| format!("failed to remove {}", script_path.display()))?;
            println!("  Removed {}", script_path.display());
        }
    }

    // 3. Clean up empty shell/ directory (best-effort)
    let shell_dir = config_dir.join("shell");
    if shell_dir.exists() {
        let _ = fs::remove_dir(&shell_dir); // only succeeds if empty
    }

    println!("\nghost-complete uninstalled successfully!");
    Ok(())
}

pub fn run_install(dry_run: bool) -> Result<()> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let zshrc = home.join(".zshrc");
    let config_dir = gc_config::config_dir().context("could not determine home directory")?;

    if dry_run {
        println!("Dry run: ghost-complete install\n");
    } else {
        println!("Installing ghost-complete...\n");
    }
    install_to(&zshrc, &config_dir, dry_run)
}

pub fn run_uninstall() -> Result<()> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    let zshrc = home.join(".zshrc");
    let config_dir = gc_config::config_dir().context("could not determine home directory")?;

    println!("Uninstalling ghost-complete...\n");
    uninstall_from(&zshrc, &config_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_remove_block_basic() {
        let content = "before\n# >>> ghost-complete initialize >>>\nstuff\n# <<< ghost-complete initialize <<<\nafter\n";
        let (result, found) = remove_block(content, INIT_BEGIN, INIT_END);
        assert!(found);
        assert_eq!(result, "before\nafter\n");
        assert!(!result.contains("ghost-complete initialize"));
    }

    #[test]
    fn test_remove_block_not_found() {
        let content = "just some shell config\nexport FOO=bar\n";
        let (result, found) = remove_block(content, INIT_BEGIN, INIT_END);
        assert!(!found);
        assert_eq!(result, content);
    }

    #[test]
    fn test_init_block_content() {
        let block = init_block();
        assert!(block.contains(INIT_BEGIN));
        assert!(block.contains(INIT_END));
        assert!(block.contains(MANAGED_WARNING));
        assert!(block.contains("exec ghost-complete"));
        // Allowlist: case statement with supported terminals
        assert!(block.contains("case \"$TERM_PROGRAM\""));
        assert!(block.contains("ghostty)"));
        // iTerm/Terminal.app: gated behind experimental config grep
        assert!(block.contains("iTerm.app|Apple_Terminal)"));
        assert!(block.contains("multi_terminal"));
        assert!(block.contains("config.toml"));
        // File-existence check before grep (prevents silent permission errors)
        assert!(block.contains("-f \"$_gc_config\""));
        assert!(block.contains("GHOST_COMPLETE_ACTIVE"));
        // tmux detection: TMUX + PPID guard
        assert!(block.contains("$TMUX"));
        assert!(block.contains("$PPID"));
        assert!(block.contains("ghost-complete\""));
        // tmux-in-Ghostty: GHOSTTY_RESOURCES_DIR
        assert!(block.contains("$GHOSTTY_RESOURCES_DIR"));
        // tmux-in-iTerm2: ITERM_SESSION_ID
        assert!(block.contains("$ITERM_SESSION_ID"));
        // Cleanup: unset the config var so it doesn't leak into user's shell
        assert!(block.contains("unset _gc_config"));
    }

    #[test]
    fn test_shell_integration_block_content() {
        let path = Path::new("/some/path/ghost-complete.zsh");
        let block = shell_integration_block(path);
        assert!(block.contains(SHELL_BEGIN));
        assert!(block.contains(SHELL_END));
        assert!(block.contains(MANAGED_WARNING));
        assert!(block.contains("/some/path/ghost-complete.zsh"));
    }

    #[test]
    fn test_install_creates_files() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        install_to(&zshrc, &config, false).unwrap();

        // .zshrc should exist with both blocks
        let content = fs::read_to_string(&zshrc).unwrap();
        assert!(content.contains(INIT_BEGIN));
        assert!(content.contains(INIT_END));
        assert!(content.contains(SHELL_BEGIN));
        assert!(content.contains(SHELL_END));
        assert!(content.contains("GHOST_COMPLETE_ACTIVE"));

        // Zsh shell script should be written
        let script = config.join("shell/ghost-complete.zsh");
        assert!(script.exists());
        let script_content = fs::read_to_string(&script).unwrap();
        assert_eq!(script_content, ZSH_INTEGRATION);

        // Bash/fish are not deployed
        assert!(!config.join("shell/ghost-complete.bash").exists());
        assert!(!config.join("shell/ghost-complete.fish").exists());

        // Source path in .zshrc must match actual script location
        let expected_source = format!("source \"{}\"", script.display());
        assert!(
            content.contains(&expected_source),
            "source path mismatch: .zshrc does not contain '{}'",
            expected_source
        );
    }

    #[test]
    fn test_install_no_existing_zshrc() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        // .zshrc doesn't exist yet
        assert!(!zshrc.exists());
        install_to(&zshrc, &config, false).unwrap();

        let content = fs::read_to_string(&zshrc).unwrap();
        assert!(content.contains(INIT_BEGIN));
        assert!(content.contains(SHELL_BEGIN));
    }

    #[test]
    fn test_install_preserves_existing_content() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        let existing = "export PATH=\"/usr/local/bin:$PATH\"\nalias ll='ls -la'\n";
        fs::write(&zshrc, existing).unwrap();

        install_to(&zshrc, &config, false).unwrap();

        let content = fs::read_to_string(&zshrc).unwrap();
        assert!(content.contains("export PATH=\"/usr/local/bin:$PATH\""));
        assert!(content.contains("alias ll='ls -la'"));
        assert!(content.contains(INIT_BEGIN));
        assert!(content.contains(SHELL_BEGIN));

        // Init block should be before user content
        let init_pos = content.find(INIT_BEGIN).unwrap();
        let user_pos = content.find("export PATH").unwrap();
        let shell_pos = content.find(SHELL_BEGIN).unwrap();
        assert!(init_pos < user_pos);
        assert!(user_pos < shell_pos);
    }

    #[test]
    fn test_idempotency() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        let existing = "export FOO=bar\n";
        fs::write(&zshrc, existing).unwrap();

        install_to(&zshrc, &config, false).unwrap();
        let first = fs::read_to_string(&zshrc).unwrap();

        install_to(&zshrc, &config, false).unwrap();
        let second = fs::read_to_string(&zshrc).unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn test_uninstall_removes_blocks() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        let existing = "export FOO=bar\n";
        fs::write(&zshrc, existing).unwrap();

        // Install then uninstall
        install_to(&zshrc, &config, false).unwrap();
        uninstall_from(&zshrc, &config).unwrap();

        // Blocks should be gone
        let content = fs::read_to_string(&zshrc).unwrap();
        assert!(!content.contains(INIT_BEGIN));
        assert!(!content.contains(SHELL_BEGIN));
        assert!(content.contains("export FOO=bar"));

        // Zsh script should be removed
        assert!(!config.join("shell/ghost-complete.zsh").exists());
    }

    #[test]
    fn test_install_creates_backup() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        let existing = "export ORIGINAL=true\n";
        fs::write(&zshrc, existing).unwrap();

        install_to(&zshrc, &config, false).unwrap();

        // with_extension replaces .zshrc extension
        let backup = zshrc.with_extension("backup.ghost-complete");
        let backup_content = fs::read_to_string(&backup).unwrap();
        assert_eq!(backup_content, existing);
    }

    #[test]
    fn test_install_creates_default_config() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        install_to(&zshrc, &config, false).unwrap();

        let config_path = config.join("config.toml");
        assert!(config_path.exists());
        let content = fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("[keybindings]"));
        assert!(content.contains("[trigger]"));
        assert!(content.contains("[popup]"));
        assert!(content.contains("[theme]"));
        // Should parse as valid TOML config (all theme fields are commented out)
        let parsed: gc_config::GhostConfig = toml::from_str(&content).unwrap();
        assert_eq!(parsed.keybindings.accept, "tab");
        assert_eq!(parsed.theme.selected, "");
        assert_eq!(parsed.theme.description, "");
    }

    #[test]
    fn test_install_does_not_clobber_existing_config() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        fs::create_dir_all(&config).unwrap();
        let config_path = config.join("config.toml");
        let custom = "[keybindings]\naccept = \"enter\"\n";
        fs::write(&config_path, custom).unwrap();

        install_to(&zshrc, &config, false).unwrap();

        let content = fs::read_to_string(&config_path).unwrap();
        assert_eq!(content, custom);
    }

    #[test]
    fn test_copy_embedded_specs() {
        let config_dir = TempDir::new().unwrap();

        copy_specs(config_dir.path()).unwrap();

        let dest = config_dir.path().join("specs");
        assert!(dest.exists());

        // All embedded specs should be written
        for (name, _) in EMBEDDED_SPECS {
            assert!(
                dest.join(name).exists(),
                "expected spec {name} to be installed"
            );
        }
        assert_eq!(
            fs::read_dir(&dest).unwrap().count(),
            EMBEDDED_SPECS.len(),
            "spec count mismatch"
        );
    }

    #[test]
    fn test_install_readonly_zshrc_succeeds() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        // Create a read-only .zshrc
        fs::write(&zshrc, "export FOO=bar\n").unwrap();
        fs::set_permissions(&zshrc, fs::Permissions::from_mode(0o444)).unwrap();

        // Install should succeed (graceful fallback, not error)
        let result = install_to(&zshrc, &config, false);
        assert!(result.is_ok());

        // File deployments should still have happened
        assert!(config.join("shell/ghost-complete.zsh").exists());
        assert!(config.join("specs").exists());
    }

    #[test]
    fn test_install_dry_run_no_writes() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        install_to(&zshrc, &config, true).unwrap();

        // Nothing should have been created
        assert!(!zshrc.exists());
        assert!(!config.exists());
    }

    #[test]
    fn test_install_dry_run_existing_files_untouched() {
        let dir = TempDir::new().unwrap();
        let zshrc = dir.path().join(".zshrc");
        let config = dir.path().join("config");

        let existing = "export FOO=bar\n";
        fs::write(&zshrc, existing).unwrap();

        fs::create_dir_all(&config).unwrap();
        let config_path = config.join("config.toml");
        let custom_config = "[keybindings]\naccept = \"enter\"\n";
        fs::write(&config_path, custom_config).unwrap();

        install_to(&zshrc, &config, true).unwrap();

        // .zshrc should be unchanged
        assert_eq!(fs::read_to_string(&zshrc).unwrap(), existing);
        // config should be unchanged
        assert_eq!(fs::read_to_string(&config_path).unwrap(), custom_config);
        // No shell scripts should have been created
        assert!(!config.join("shell").exists());
    }
}
