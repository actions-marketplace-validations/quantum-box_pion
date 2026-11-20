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
| L2 crate の中 | 単体テストのうち、変えた関数や型に依存するのはどれか | rust-analyzer の索引（SCIP）から作る参照グラフ | 実装済み |
| L3 API テスト | シナリオのうち、変えた関数や型に依存する endpoint を叩くのはどれか | シナリオの method と path → handler → 参照グラフ | 実装済み（GraphQL は名前による推測） |

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

`Cargo.lock` の変更で全 crate に戻る 14 commit は、その後の対応で絞れるようにした。
lock のどの package の解決が変わったかを見て、それに依存する crate だけを選ぶ。

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

### 索引から分かったこと

fixture と tachyon-apps に実際に rust-analyzer をかけて確かめた。

- 定義には、item 全体の範囲（属性・署名・本体）が付く。変更した行は、その行を含む
  一番内側の定義に対応させる。
- `#[test]` は、item の範囲の中に `test` という名前の属性 macro への参照として現れる。
  `tokio::test` や `sqlx::test` も同じ名前で拾える。
- trait の impl と trait の method を結ぶ情報は、索引には入っていない。symbol の文字列
  （`impl#[Type][Trait]method().` と `Trait#method().`）から結ぶ。trait は名前だけで
  書かれるので、同名の trait が複数あれば全部に結ぶ。
- 属性 macro の付いた関数の中に書いた item（`#[tokio::test]` の中の mock）は、外側の
  関数と同じ範囲を持つ。関数の中の item は外から名前で参照できないので、その範囲の
  中の変更と参照は、外側の関数（名前が最初に出てくる定義）のものとして扱う。
- derive などの macro が生成した定義は、source に場所がないので索引に出ない。そこを
  通る参照は辿れない。選び漏れの原因になりうるので、`audit` で数える。

### tachyon-apps で測った結果（2026-11-21）

172 crate、テスト約 17,000 件。

| 項目 | 結果 |
| --- | --- |
| 索引を作る時間 | 26 分（1,590 秒） |
| 索引を作るときの memory | 最大 19 GB |
| 索引の大きさ | 344 MB |
| 索引を読んで計画を出す時間 | 数秒（300 件の計画を含めて 5 秒） |

無作為に選んだ 300 行について、「この行を変えたら何が流れるか」を出した。

| 項目 | 結果 |
| --- | --- |
| 絞れた変更 | 300 件中 295 件。残り 5 件は crate 全部に戻った |
| テスト以外の code を変えたときに選ぶテスト | 中央値 6、75% 点 77、90% 点 347、最大 約 5,000 |
| crate 単位（L1）で全部流すことになる crate の数 | 中央値 6 |
| 選ぶシナリオ | 中央値 46（全 178 件） |

- 46 件は、handler に結べない request を含むシナリオ。その crate が影響を受けると
  必ず流れる。route の path が定数で書かれている箇所を読めるようにすると減る。
- テスト関数の中だけを変えた場合は、シナリオを流さない。
- rust-analyzer は `Encountered enclosing definition with no name` を 3,325 件出した。
  関数の中の item の「外側の定義」の名前を記録できなかった、という報告で、定義自体は
  索引に入っている。pion はその欄を読まない。`cargo pion index` は、こうした報告を
  種類ごとの件数にまとめ、全文は log に書く。

### 索引はどの revision で作るか

索引づくりに 26 分かかるので、変更のたびには作れない。main の commit ごとに CI で
1 回作り、変更は base の索引に対して計画する（`--scip-revision base`）。

- 変更した行は、変更前の行番号で、変更前の索引の定義に対応させる。
- 新しく足した code は、それを呼ぶために書き換えた既存の行から辿る。
- 既存の item の間に新しい item を足した場合は、その crate 全部に戻す。新しいテストを
  足す変更がこれに当たる。ここは次に絞りたいところ。
- 19 GB の memory が要るので、索引を作る runner は大きいものが要る。

## L3：API テスト

- シナリオの YAML には method と path が書いてある。これは静的に読める。
- path から handler への対応は、router の登録か OpenAPI の定義から作る。
- handler から先は、単体テストと同じ参照グラフを辿る。
- GraphQL は、シナリオの query に出てくる field から resolver の関数に対応させる。

## モデル

型は `src/` にある。主なものは次のとおり。

```rust
// 変更（change.rs, git.rs）
struct ChangeSet { files: Vec<ChangedFile> }
struct ChangedFile { path: RepoPath, kind: FileKind }
enum FileKind { RustSource, CrateManifest, CrateData, WorkspaceGlobal, Outside }
struct ChangedLine { line, gap_to, text, addition_only }   // base 側か head 側の行番号

// L1（workspace.rs, lockfile.rs）
struct WorkspaceGraph { /* guppy */ }
enum LockChange { Packages(Vec<LockedPackage>), Everything }

// L2（scip.rs, symbols.rs）
struct Index { documents: Vec<Document> }                  // SCIP を自前で読む
struct ReferenceGraph { /* 定義 -> それが変わると影響を受ける定義 */ }
struct SymbolId(String);                                   // SCIP の symbol
struct TestCase { package: PackageName, name: Option<TestName> }

// L3（routes.rs, scenario.rs）
struct RouteTable { /* method, path -> handler */ }
struct Scenario { path, package, handlers: BTreeSet<SymbolId>, untraced: bool }

// 計画（plan.rs）
enum Selection { All, Tests(BTreeSet<TestName>) }
enum Reason { FileChanged, DependsOnCrate, DependsOnExternal, Reaches, FellBack }
enum FallbackCause { WorkspaceGlobal, NotInIndex, OutsideDefinitions, UnnamedTest }
enum ScenarioReason { FileChanged, Reaches, CrateNotNarrowed, Untraced }
struct TestPlan { crates, scenarios, uncovered }
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
2. **`Cargo.lock` の差分（済み）。** 解決が変わった package に依存する crate だけを選ぶ。
3. **参照グラフ（済み）。** rust-analyzer の索引を読み、trait の規則を入れた。
4. **crate の中を絞る（済み）。** `--scip` を付けたときに有効になる。
5. **API テストを絞る（済み）。** REST は route から、GraphQL は名前から handler を決める。
6. **選び漏れを数える（`audit` は済み、運用はこれから）。** 全テストを流す CI の結果と
   突き合わせ、選ばなかったのに失敗したテストを数える。この数字が十分小さいことを
   確かめてから、deploy の判定に使う。

## 次に直すところ

- 新しい item（新しいテスト）を足した変更を、crate 全部に戻さずに絞る。
- route の path が定数の箇所を読み、handler に結べないシナリオを減らす。
- root の `Cargo.toml` の変更を、変わった依存を使う crate だけに絞る。
- 索引を crate 単位で作って使い回し、26 分を縮める。

## まだ確かめていないこと

- 選び漏れの実数。macro が生成する code を通る参照（`sqlx::query!`、`async_trait`、
  derive、GraphQL の derive）は辿れない場合がある。`audit` で実際の失敗と突き合わせる
  まで、数字はない。
- GraphQL の「field 名 → 同名の method」という対応が、実際の resolver とどれだけ
  合っているか。
- 索引を作る runner に必要な大きさと、CI での所要時間。
