# pion 設計

変更したファイルから、影響を受けるテストだけを選ぶ。crate の間だけでなく、crate
の中の単体テストや API テストも、変更と関係ないものは流さない。

## 目的

1. **依存を正しく理解する。** 手書きの glob ではなく、Cargo の依存グラフと、コードの
   参照グラフ（誰が何を参照しているか）から判定する。
2. **関係のないテストを流さない。** crate 単位で止めず、crate の中の単体テストと API
   テストまで絞る。
3. **選び漏れを測る。** 絞った結果が正しかったかを、全テストを流す CI の結果と
   突き合わせて数字にする。

## 3段の解像度

粗い段から順に絞る。細かい段で辿れなければ、1つ上の段に戻る。

| 段 | 問い | 情報源 | 状態 |
| --- | --- | --- | --- |
| L1 crate の間 | この変更で、どの crate の振る舞いが変わりうるか | `cargo metadata` の依存グラフ（guppy） | 実装済み |
| L2 crate の中 | 単体テストのうち、変えた関数や型に依存するのはどれか | rust-analyzer の索引（SCIP）から作る参照グラフ | 未着手 |
| L3 API テスト | シナリオのうち、変えた関数や型に依存する endpoint を叩くのはどれか | シナリオの method と path → handler → 参照グラフ | 未着手 |

## L1：crate の間

`WorkspaceGraph` が `cargo metadata` を読み、変更した crate から依存を逆向きに辿る。

- 通常の依存と build の依存は、推移的に辿る。
- dev の依存は、最後の1段だけ辿る。ある crate のテストはその crate の dev の依存を
  link するが、その crate に依存する側には伝わらない。
- `Cargo.lock`、root の `Cargo.toml`、`rust-toolchain.toml`、`clippy.toml`、`.cargo/`
  は全 crate に効くものとして扱う。
- どの crate にも属さないファイルは、何も選ばず `uncovered` に載せる。deploy を
  gate する呼び出し側は、`--deny-uncovered` でこれを失敗にできる。

### tachyon-apps で測った結果（2026-11-21）

172 crate の workspace で、1回の実行は約1秒。直近 60 commit を1つずつ当てた。

| 結果 | commit 数 |
| --- | --- |
| crate を1つも選ばない（CI や docs だけの変更） | 30 |
| 全 172 crate（`Cargo.lock` など workspace 全体に効く変更） | 14 |
| 一部の crate | 16 |

一部を選んだ 16 commit では、変更した crate は中央値 1.5、選んだ crate は中央値 24
（最小 1、最大 58）。全 crate を流す場合の約 14% になる。

`Cargo.lock` の変更で全 crate に戻る 14 commit が次の改善点。lock のどの package が
変わったかを見て、それに依存する crate だけを選べる。

## L2：crate の中

Rust は crate を1つの単位として compile するので、crate の中の依存は、コードを解析
して辿る。

### 何で辿るか

| 方法 | わかること | 限界 |
| --- | --- | --- |
| `syn`（構文木だけ） | module の `use` 関係 | 名前解決も型推論もしない。method 呼び出しの先や trait の実装がわからない |
| rust-analyzer の索引（SCIP） | 名前解決と型推論を済ませた「定義」と「参照」。関数・型・定数の単位 | `dyn Trait` 越しの呼び出しは、実装ではなく trait の method への参照になる |
| rustc の内部（MIR） | 単相化した後の正確な呼び出し関係 | 不安定な内部 API に依存する |

採るのは rust-analyzer の索引。`rust-analyzer scip` が出す索引を読んでグラフを作る。

### 辿り方

1. 変更した行を、それを含む定義（関数・型・定数・impl）に対応させる。
2. その定義を参照している定義を、逆向きに辿る。
3. `#[test]` の関数に届いたら、そのテストを選ぶ。handler に届いたら、その endpoint
   を叩くシナリオを選ぶ。

### 静的に辿れないところの規則

| 場面 | 規則 |
| --- | --- |
| `Arc<dyn Repository>` のような trait object 越しの呼び出し | trait の method への参照は、その trait の全部の実装への参照として扱う |
| trait の実装を変えた | その trait の method を参照している全部の定義に伝える |
| `sqlx::query!` の SQL、`include_str!` のファイル | macro の引数からファイルを読み取り、参照として足す。読めなければ crate 全部に戻す |
| 登録型の仕組み（route の一覧、DI の組み立て） | 組み立てをしている関数を変えたら、その binary の API テスト全部 |
| proc macro の実装を変えた | その macro を使っている crate 全部 |

## L3：API テスト

- シナリオの YAML には method と path が書いてある。これは静的に読める。
- path から handler への対応は、router の登録か OpenAPI の定義から作る。
- handler から先は、単体テストと同じ参照グラフを辿る。
- GraphQL は、シナリオの query に出てくる field から resolver の関数に対応させる。

## モデル

実装済みの型は `src/` にある。L2・L3 で足す予定の型を含めた全体は次のとおり。

```rust
// 変更
struct ChangeSet { files: Vec<ChangedFile> }
struct ChangedFile { path: RepoPath, kind: FileKind }      // L2 で hunks を足す
enum FileKind { RustSource, CrateManifest, CrateData, WorkspaceGlobal, Outside }

// L1
struct WorkspaceGraph { /* guppy */ }

// L2・L3（未実装）
struct Definition { id: SymbolId, kind: DefKind, package: PackageName, span: FileSpan }
enum DefKind { Function, Method, Type, Const, Trait, TraitMethod, Impl, Test, Handler }
struct Edge { from: SymbolId, to: SymbolId, via: EdgeKind }
enum EdgeKind { Direct, TraitDispatch, ImplOfTrait, DataFile, Registration }
struct Scenario { id: ScenarioId, calls: Vec<Endpoint>, unresolved: bool }

// 計画
enum Selection { All /* L2 で Only(tests) を足す */ }
enum Reason { FileChanged, DependsOnCrate, FellBack /* L2 で Reaches(道筋) を足す */ }
struct TestPlan { crates: BTreeMap<PackageName, CratePlan>, uncovered: Vec<RepoPath> }
```

選んだ理由は必ず `Reason` として残す。「このテストはなぜ流れたか」を説明でき、
絞れなかった理由（`FallbackCause`）を数えて、多いものから規則を足していける。

## 先にある道具

同じ問題を解こうとしている crate がある。依存には入れず、設計の参考にだけ使う。

| crate | 方法 | 参考にするところ |
| --- | --- | --- |
| `cargo-impact` | 静的。`syn` と rust-analyzer | trait の変更の分け方、`dyn Trait` の扱い、確からしさの段階 |
| `cargo-affected` | nextest でテストごとに coverage を取る | 型や定数の変更の戻し方、誤って選ぶ場合と選び漏れる場合を分けた説明 |
| `cargo-difftests` | テストごとの coverage | 更新が止まっている |

自作する理由：この道具は CI の中で、repo の全コードと deploy の権限の近くで動く。
利用者の少ない crate を入れると、その作者と、その依存の全部を信頼することになる。

## 段階

1. **L1（済み）。** crate の依存で選ぶ。nextest の filterset を出す。
2. **`Cargo.lock` の差分。** 変わった外部 package に依存する crate だけを選ぶ。
3. **参照グラフを作る。** rust-analyzer の索引を読み、trait の規則を入れる。まだ
   絞らず、「絞っていたらどうなったか」を全テストの結果と突き合わせる。
4. **crate の中を絞る。** 選び漏れが十分少ないことを数字で確かめてから有効にする。
5. **API テストを絞る。** シナリオの method と path を handler に対応させる。

## 確かめること

- `rust-analyzer scip` が大きな workspace で何分かかり、どれだけ memory を使うか。
- trait の規則（全部の実装に伝える）で、どれだけ絞れるか。
- macro の中の参照（`sqlx::query!`、`async_trait`、GraphQL の derive）がどこまで
  索引に出るか。
