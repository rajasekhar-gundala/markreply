use axum::{
    extract::{Path, Query, Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Response},
    Form,
};
use sqlx::SqlitePool;

// 🌟 NEW: Import LoginForm
use crate::models::{Comment, CommentQuery, CreateCommentForm, LoginForm};

/* --------------------------------------------------------
   PUBLIC API ROUTES (GET & POST COMMENTS)
-------------------------------------------------------- */

pub async fn get_comments(
    State(pool): State<SqlitePool>,
    Query(query): Query<CommentQuery>,
) -> impl IntoResponse {
    let comments: Vec<Comment> = sqlx::query_as!(
        Comment,
        // 🌟 FIX: Changed ASC to DESC at the end of the query
        "SELECT id, post_slug, author_name, author_email, content, is_approved, created_at, parent_id FROM comments WHERE post_slug = ? AND is_approved = 1 ORDER BY created_at DESC",
        query.slug
    )
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    if comments.is_empty() {
        return Html(
            "<p class=\"no-comments\">No comments yet. Be the first to share your thoughts!</p>"
                .to_string(),
        );
    }

    // 🌟 NEW: Group comments into threads
    let mut top_level = Vec::new();
    let mut replies = std::collections::HashMap::new();

    for c in comments {
        if let Some(ref pid) = c.parent_id {
            replies.entry(pid.clone()).or_insert_with(Vec::new).push(c);
        } else {
            top_level.push(c);
        }
    }

    let mut html_output = String::new();
    for parent in top_level {
        html_output.push_str(&render_comment_item(&parent, false));
        // Render replies directly underneath the parent
        if let Some(children) = replies.get(&parent.id) {
            for child in children {
                html_output.push_str(&render_comment_item(child, true));
            }
        }
    }

    Html(html_output)
}

pub async fn post_comment(
    State(pool): State<SqlitePool>,
    Form(payload): Form<CreateCommentForm>,
) -> impl IntoResponse {
    // 1. Spam Trap Check
    if let Some(bot_trap) = payload.honeypot {
        if !bot_trap.trim().is_empty() {
            return (StatusCode::OK, Html(String::new())).into_response();
        }
    }

    let clean_author = ammonia::clean(&payload.author_name);
    let clean_content = ammonia::clean(&payload.content);
    let id = uuid::Uuid::new_v4().to_string();

    if clean_content.trim().is_empty() || clean_author.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Html(
                "<p style=\"color: red;\">Author name and comment cannot be empty.</p>".to_string(),
            ),
        )
            .into_response();
    }

    // 🌟 NEW: Check if the submitter is the Admin
    let admin_email = std::env::var("ADMIN_EMAIL").unwrap_or_default();
    let is_admin = match &payload.author_email {
        Some(email) if !email.trim().is_empty() && !admin_email.is_empty() => {
            email.trim().eq_ignore_ascii_case(&admin_email)
        }
        _ => false,
    };

    // SQLite uses 1 for true and 0 for false
    let is_approved_db = if is_admin { 1 } else { 0 };

    // 🌟 FIX: Pass is_approved_db to the query instead of hardcoding 0
    let res = sqlx::query!(
        "INSERT INTO comments (id, post_slug, author_name, author_email, content, is_approved, parent_id) VALUES (?, ?, ?, ?, ?, ?, ?)",
        id,
        payload.post_slug,
        clean_author,
        payload.author_email,
        clean_content,
        is_approved_db,
        payload.parent_id
    )
    .execute(&pool)
    .await;

    match res {
        Ok(_) => {
            let created_comment = Comment {
                id,
                post_slug: payload.post_slug,
                author_name: clean_author,
                author_email: payload.author_email,
                content: clean_content,
                is_approved: is_admin, // 🌟 Save state to the struct
                created_at: chrono::Utc::now().naive_utc(),
                parent_id: payload.parent_id,
            };

            // Only send an email alert if someone ELSE left a comment
            if !is_admin {
                let comment_clone = created_comment.clone();
                tokio::spawn(async move {
                    crate::mailer::send_new_comment_alert(&comment_clone).await;
                });
            }

            // 🌟 NEW: Provide a different success message based on approval status
            let success_msg = if is_admin {
                "<div class=\"comment-success-msg\">Welcome back, Admin! Your comment has been posted instantly.</div>".to_string()
            } else {
                "<div class=\"comment-success-msg\">Thank you! Your comment has been submitted and is awaiting moderation.</div>".to_string()
            };

            (StatusCode::CREATED, Html(success_msg)).into_response()
        }
        Err(e) => {
            tracing::error!("Failed to save comment: {}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Html("<p style=\"color: red;\">Failed to save comment.</p>".to_string()),
            )
                .into_response()
        }
    }
}

// 🌟 FIX: Updated renderer to parse Markdown and inject Admin badges
fn render_comment_item(c: &Comment, is_reply: bool) -> String {
    let wrapper_class = if is_reply {
        "comment-item comment-reply"
    } else {
        "comment-item"
    };

    let reply_btn = if !is_reply {
        format!("<button type=\"button\" class=\"mr-reply-btn\" onclick=\"window.mrReplyTo('{}')\">↳ Reply</button>", c.id)
    } else {
        "".to_string()
    };

    // 🌟 NEW: Check if this comment belongs to the Admin
    let admin_email = std::env::var("ADMIN_EMAIL").unwrap_or_default();
    let is_admin = match &c.author_email {
        Some(email) if !email.trim().is_empty() && !admin_email.is_empty() => {
            email.trim().eq_ignore_ascii_case(&admin_email)
        }
        _ => false,
    };

    let admin_badge = if is_admin {
        "<span class=\"mr-admin-badge\">Admin</span>"
    } else {
        ""
    };

    // 🌟 NEW: Parse the raw Markdown into HTML, then sanitize it again to be safe
    let parser = pulldown_cmark::Parser::new(&c.content);
    let mut html_content = String::new();
    pulldown_cmark::html::push_html(&mut html_content, parser);
    let safe_html = ammonia::clean(&html_content);

    format!(
        "<div class=\"{wrapper_class}\">\
            <div class=\"comment-header\">\
                <div>\
                    <span class=\"comment-author\">{name}</span>\
                    {badge}\
                </div>\
                <span class=\"comment-date\">{date}</span>\
            </div>\
            <div class=\"comment-content\">{content}</div>\
            {reply_btn}\
            <div id=\"mr-slot-{id}\" class=\"mr-form-slot\"></div>\
        </div>",
        wrapper_class = wrapper_class,
        name = c.author_name,
        badge = admin_badge,
        date = c.created_at.format("%b %d, %Y at %H:%M"),
        content = safe_html, // Injected parsed HTML instead of raw text
        reply_btn = reply_btn,
        id = c.id
    )
}

/* --------------------------------------------------------
   AUTHENTICATION LOGIC & LOGIN PAGE
-------------------------------------------------------- */

// 🌟 UPDATED: Redirects to /admin/login instead of throwing a blank 401 error
#[allow(clippy::result_large_err)]
pub async fn require_admin_auth(req: Request, next: Next) -> Result<Response, Response> {
    let expected_token =
        std::env::var("ADMIN_TOKEN").unwrap_or_else(|_| "secret-admin-key".to_string());

    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|val| val.to_str().ok())
        .map(|val| val.trim_start_matches("Bearer ").to_string());
    let cookie_token = req
        .headers()
        .get(header::COOKIE)
        .and_then(|val| val.to_str().ok())
        .and_then(|cookie_str| {
            cookie_str.split(';').find_map(|c| {
                let mut parts = c.trim().split('=');
                if parts.next()? == "admin_token" {
                    Some(parts.next()?.to_string())
                } else {
                    None
                }
            })
        });

    if auth_header.as_deref() == Some(&expected_token)
        || cookie_token.as_deref() == Some(&expected_token)
    {
        Ok(next.run(req).await)
    } else {
        // Redirect unauthorized users
        Err((StatusCode::SEE_OTHER, [(header::LOCATION, "/admin/login")]).into_response())
    }
}

// 🌟 NEW: Renders the Login UI
pub async fn admin_login_form() -> impl IntoResponse {
    Html(render_login_page(None))
}

// 🌟 FIX 1: Added .into_response() to the else block
pub async fn admin_login_submit(Form(payload): Form<LoginForm>) -> impl IntoResponse {
    let expected = std::env::var("ADMIN_TOKEN").unwrap_or_else(|_| "secret-admin-key".to_string());

    if payload.token == expected {
        let cookie = format!(
            "admin_token={}; Path=/; HttpOnly; Max-Age=31536000",
            payload.token
        );
        (
            StatusCode::SEE_OTHER,
            [
                (header::SET_COOKIE, cookie),
                (header::LOCATION, "/admin".to_string()),
            ],
        )
            .into_response()
    } else {
        Html(render_login_page(Some("Invalid Token. Please try again."))).into_response()
    }
}

// 🌟 FIX 2: Removed .to_string() from the location header
pub async fn admin_logout() -> impl IntoResponse {
    let cookie = "admin_token=; Path=/; HttpOnly; Max-Age=0"; // Kills the cookie
    (
        StatusCode::SEE_OTHER,
        [
            (header::SET_COOKIE, cookie),
            (header::LOCATION, "/admin/login"),
        ],
    )
        .into_response()
}

fn render_login_page(error: Option<&str>) -> String {
    let err_msg = match error {
        Some(msg) => format!("<div class=\"p-3 mb-4 rounded bg-rose-950/50 border border-rose-900 text-rose-400 text-sm\">{}</div>", msg),
        None => "".to_string(),
    };

    format!("\
    <!DOCTYPE html>\
    <html lang=\"en\">\
    <head>\
        <meta charset=\"UTF-8\">\
        <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\
        <title>Admin Login</title>\
        <script src=\"https://cdn.tailwindcss.com\"></script>\
    </head>\
    <body class=\"bg-neutral-950 text-neutral-100 min-h-screen flex items-center justify-center font-sans p-6\">\
        <div class=\"bg-neutral-900 border border-neutral-800 p-8 rounded-xl shadow-lg w-full max-w-md\">\
            <h1 class=\"text-2xl font-bold text-emerald-400 mb-6\">Admin Login</h1>\
            {error_html}\
            <form method=\"POST\" action=\"/admin/login\" class=\"space-y-4\">\
                <div>\
                    <label class=\"block text-xs font-semibold mb-1 text-neutral-400\">Authentication Token</label>\
                    <input type=\"password\" name=\"token\" required class=\"w-full px-3.5 py-2 bg-neutral-950 border border-neutral-800 rounded-lg text-sm outline-none focus:ring-2 focus:ring-emerald-500 text-white\" />\
                </div>\
                <button type=\"submit\" class=\"w-full px-5 py-2.5 bg-emerald-600 hover:bg-emerald-500 text-white font-bold rounded-lg text-sm transition-colors shadow-sm\">\
                    Secure Login\
                </button>\
            </form>\
        </div>\
    </body>\
    </html>\
    ", error_html = err_msg)
}

/* --------------------------------------------------------
   ADMIN DASHBOARD ROUTES
-------------------------------------------------------- */

pub async fn admin_dashboard(State(_pool): State<SqlitePool>) -> impl IntoResponse {
    // 🌟 UPDATED: Added a Logout button to the header
    let html = "\
    <!DOCTYPE html>\
    <html lang=\"en\">\
    <head>\
        <meta charset=\"UTF-8\">\
        <meta name=\"viewport\" content=\"width=device-width, initial-scale=1.0\">\
        <title>Comment Moderation</title>\
        <script src=\"https://cdn.tailwindcss.com\"></script>\
        <script src=\"https://unpkg.com/htmx.org@1.9.12\"></script>\
    </head>\
    <body class=\"bg-neutral-950 text-neutral-100 min-h-screen font-sans p-6\">\
        <!-- 🌟 CHANGED: max-w-6xl is now max-w-7xl below -->\
        <div class=\"max-w-7xl mx-auto space-y-6\">\
            <header class=\"flex items-center justify-between border-b border-neutral-800 pb-4\">\
                <div>\
                    <h1 class=\"text-2xl font-bold text-emerald-400\">Comment Moderation</h1>\
                    <p class=\"text-xs text-neutral-400 mt-1\">Review, approve, and purge blog comments</p>\
                </div>\
                <div class=\"flex gap-2\">\
                    <button hx-get=\"/admin/api/comments\" hx-target=\"#comment-table-body\" class=\"px-3 py-1.5 bg-neutral-800 hover:bg-neutral-700 text-xs font-semibold rounded-lg transition-colors\">Refresh</button>\
                    <a href=\"/admin/logout\" class=\"px-3 py-1.5 bg-rose-950 hover:bg-rose-900 text-rose-300 border border-rose-800 text-xs font-semibold rounded-lg transition-colors\">Logout</a>\
                </div>\
            </header>\
            <div class=\"bg-neutral-900 border border-neutral-800 rounded-xl overflow-hidden shadow-lg\">\
                <div class=\"overflow-x-auto\">\
                    <table class=\"w-full text-left text-sm\">\
                        <thead class=\"bg-neutral-800/60 text-xs uppercase text-neutral-400 border-b border-neutral-800\">\
                            <tr>\
                                <th class=\"px-4 py-3\">Status</th>\
                                <th class=\"px-4 py-3\">Post Slug</th>\
                                <th class=\"px-4 py-3\">Author</th>\
                                <th class=\"px-4 py-3\">Comment</th>\
                                <th class=\"px-4 py-3\">Date</th>\
                                <th class=\"px-4 py-3 text-right\">Actions</th>\
                            </tr>\
                        </thead>\
                        <tbody id=\"comment-table-body\" hx-get=\"/admin/api/comments\" hx-trigger=\"load\" class=\"divide-y divide-neutral-800\">\
                            <tr>\
                                <td colspan=\"6\" class=\"p-6 text-center text-neutral-500 animate-pulse\">Loading comments...</td>\
                            </tr>\
                        </tbody>\
                    </table>\
                </div>\
            </div>\
        </div>\
    </body>\
    </html>\
    ";

    Html(html.to_string())
}

pub async fn list_admin_comments(State(pool): State<SqlitePool>) -> impl IntoResponse {
    let comments: Vec<Comment> = sqlx::query_as!(
        Comment,
        // 🌟 FIX: Added parent_id to the SELECT statement
        "SELECT id, post_slug, author_name, author_email, content, is_approved, created_at, parent_id FROM comments ORDER BY created_at DESC LIMIT 100"
    ).fetch_all(&pool).await.unwrap_or_default();

    if comments.is_empty() {
        return Html("<tr><td colspan=\"6\" class=\"p-6 text-center text-neutral-500\">No comments found.</td></tr>".to_string());
    }

    let mut rows = String::new();
    for comment in comments {
        rows.push_str(&render_admin_row(&comment));
    }
    Html(rows)
}

pub async fn toggle_approve_comment(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let _ = sqlx::query!(
        "UPDATE comments SET is_approved = NOT is_approved WHERE id = ?",
        id
    )
    .execute(&pool)
    .await;

    // 🌟 FIX: Explicitly select all columns including parent_id instead of using SELECT *
    if let Ok(comment) = sqlx::query_as!(
        Comment,
        "SELECT id, post_slug, author_name, author_email, content, is_approved, created_at, parent_id FROM comments WHERE id = ?",
        id
    ).fetch_one(&pool).await {
        Html(render_admin_row(&comment))
    } else {
        Html("".to_string())
    }
}

pub async fn delete_comment(
    State(pool): State<SqlitePool>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let _ = sqlx::query!("DELETE FROM comments WHERE id = ?", id)
        .execute(&pool)
        .await;
    Html("".to_string())
}

fn render_admin_row(c: &Comment) -> String {
    let status_badge = if c.is_approved {
        "<span class=\"inline-flex items-center px-2 py-0.5 rounded-full text-xs font-semibold bg-emerald-950 text-emerald-400 border border-emerald-800\">Approved</span>"
    } else {
        "<span class=\"inline-flex items-center px-2 py-0.5 rounded-full text-xs font-semibold bg-amber-950 text-amber-400 border border-amber-800\">Pending</span>"
    };

    let type_badge = if c.parent_id.is_some() {
        "<span class=\"inline-flex items-center px-2 py-0.5 rounded-full text-xs font-semibold bg-indigo-950 text-indigo-400 border border-indigo-800\">Reply</span>"
    } else {
        "<span class=\"inline-flex items-center px-2 py-0.5 rounded-full text-xs font-semibold bg-slate-800 text-slate-400 border border-slate-700\">Top-Level</span>"
    };

    let toggle_label = if c.is_approved {
        "Unapprove"
    } else {
        "Approve"
    };

    format!(
        "<tr id=\"comment-row-{id}\" class=\"hover:bg-neutral-800/40 transition-colors\">\
            <td class=\"px-4 py-3 whitespace-nowrap\">\
                <div class=\"flex items-center gap-2\">\
                    {badge}\
                    {type_badge}\
                </div>\
            </td>\
            <td class=\"px-4 py-3 font-mono text-xs text-neutral-300 max-w-[12rem] truncate\" title=\"{slug}\">{slug}</td>\
            <td class=\"px-4 py-3 max-w-[12rem]\">\
                <div class=\"font-bold text-neutral-200 truncate\" title=\"{author}\">{author}</div>\
                <div class=\"text-xs text-neutral-500 truncate\" title=\"{email}\">{email}</div>\
            </td>\
            <!-- 🌟 FIX: Replaced 'truncate' with 'line-clamp-2' and 'whitespace-normal' in a wrapper div -->\
            <td class=\"px-4 py-3 text-neutral-300 max-w-xs xl:max-w-md\">\
                <div class=\"line-clamp-2 whitespace-normal break-words\">{content}</div>\
            </td>\
            <td class=\"px-4 py-3 text-xs text-neutral-400 whitespace-nowrap\">{date}</td>\
            <td class=\"px-4 py-3 text-right space-x-2 whitespace-nowrap\">\
                <button hx-post=\"/admin/api/comments/{id}/toggle\" hx-target=\"#comment-row-{id}\" hx-swap=\"outerHTML\" class=\"px-2.5 py-1 text-xs font-semibold bg-neutral-800 hover:bg-neutral-700 text-neutral-200 rounded border border-neutral-700 transition-colors\">{toggle}</button>\
                <button hx-delete=\"/admin/api/comments/{id}\" hx-target=\"#comment-row-{id}\" hx-swap=\"outerHTML\" hx-confirm=\"Are you sure you want to permanently delete this comment?\" class=\"px-2.5 py-1 text-xs font-semibold bg-rose-950 hover:bg-rose-900 text-rose-300 rounded border border-rose-800 transition-colors\">Delete</button>\
            </td>\
        </tr>",
        id = c.id,
        badge = status_badge,
        type_badge = type_badge,
        slug = c.post_slug,
        author = c.author_name,
        email = c.author_email.as_deref().unwrap_or("-"),
        content = c.content,
        date = c.created_at.format("%Y-%m-%d %H:%M"),
        toggle = toggle_label
    )
}

pub async fn serve_css() -> impl axum::response::IntoResponse {
    let css = r###"
    /* 🌟 Base Variables (Light Mode) */
    :root {
        --mr-primary: #059669;
        --mr-bg: #ffffff;
        --mr-input-bg: #f9fafb;
        --mr-border: #e5e7eb;
        --mr-text: #171717;
        --mr-muted: #737373;
    }

    @media (prefers-color-scheme: dark) {
        :root:not(.light) {
            --mr-bg: #171717;
            --mr-input-bg: #262626;
            --mr-border: #404040;
            --mr-text: #f5f5f5;
            --mr-muted: #a3a3a3;
        }
    }

    html.dark, .dark {
        --mr-bg: #171717;
        --mr-input-bg: #262626;
        --mr-border: #404040;
        --mr-text: #f5f5f5;
        --mr-muted: #a3a3a3;
    }

    /* 🌟 THE FIX: This stops inputs from overlapping their grid! */
    .mr-container, .mr-container * {
        box-sizing: border-box;
    }

    /* Widget Styles */
    .mr-container { font-family: system-ui, sans-serif; color: var(--mr-text); margin-top: 3rem; padding-top: 2rem; border-top: 1px solid var(--mr-border); }
    .mr-title { font-size: 1.5rem; font-weight: bold; margin-bottom: 1.5rem; color: var(--mr-text); }

    .mr-input {
        width: 100%;
        padding: 0.5rem 0.75rem;
        border: 1px solid var(--mr-border);
        border-radius: 0.5rem;
        background: var(--mr-input-bg);
        color: var(--mr-text);
        margin-top: 0.25rem;
        font-family: inherit;
        transition: border-color 0.2s, box-shadow 0.2s;
    }
    .mr-input::placeholder { color: var(--mr-muted); opacity: 0.7; }
    .mr-input:focus { outline: none; border-color: var(--mr-primary); box-shadow: 0 0 0 2px rgba(5, 150, 105, 0.2); }

    .mr-label { font-size: 0.75rem; font-weight: 600; color: var(--mr-text); }

    .mr-btn { background: var(--mr-primary); color: white; padding: 0.5rem 1.25rem; border-radius: 0.5rem; border: none; font-weight: bold; cursor: pointer; transition: opacity 0.2s; }
    .mr-btn:hover { opacity: 0.9; }

    .mr-grid { display: grid; grid-template-columns: 1fr; gap: 1rem; margin-bottom: 1rem; }
    @media (min-width: 640px) { .mr-grid { grid-template-columns: 1fr 1fr; } }

    /* 🌟 Markdown & Admin Badge Styles */
    .mr-admin-badge { background: var(--mr-primary); color: white; padding: 0.1rem 0.4rem; border-radius: 0.25rem; font-size: 0.65rem; margin-left: 0.5rem; vertical-align: middle; text-transform: uppercase; letter-spacing: 0.05em; }
    
    .comment-content p { margin-top: 0; margin-bottom: 0.75rem; line-height: 1.6; }
    .comment-content p:last-child { margin-bottom: 0; }
    .comment-content strong { font-weight: 700; color: var(--mr-text); }
    .comment-content em { font-style: italic; }
    .comment-content del { text-decoration: line-through; opacity: 0.7; }
    
    /* Links */
    .comment-content a { color: var(--mr-primary); text-decoration: underline; text-underline-offset: 2px; }
    .comment-content a:hover { opacity: 0.8; }
    
    /* Blockquotes */
    .comment-content blockquote { border-left: 3px solid var(--mr-border); margin: 0.75rem 0; padding-left: 1rem; color: var(--mr-muted); font-style: italic; background: rgba(0,0,0,0.02); padding-top: 0.25rem; padding-bottom: 0.25rem; }
    
    /* Lists */
    .comment-content ul, .comment-content ol { margin-top: 0.5rem; margin-bottom: 0.75rem; padding-left: 1.5rem; }
    .comment-content ul { list-style-type: disc; }
    .comment-content ol { list-style-type: decimal; }
    .comment-content li { margin-bottom: 0.25rem; }
    
    /* Code Blocks */
    .comment-content code { background: var(--mr-input-bg); padding: 0.1rem 0.3rem; border-radius: 0.25rem; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 0.85em; border: 1px solid var(--mr-border); }
    .comment-content pre { background: var(--mr-input-bg); padding: 0.75rem; border-radius: 0.5rem; overflow-x: auto; margin: 0.75rem 0; border: 1px solid var(--mr-border); }
    .comment-content pre code { background: transparent; padding: 0; border: none; font-size: 0.85em; }

    .comment-item { padding: 1rem; background: var(--mr-bg); border: 1px solid var(--mr-border); border-radius: 0.5rem; margin-bottom: 1rem; color: var(--mr-text); }
    .comment-header { display: flex; justify-content: space-between; margin-bottom: 0.5rem; }
    .comment-author { font-weight: bold; color: var(--mr-text); }
    .comment-date { font-size: 0.75rem; color: var(--mr-muted); }
    .comment-content { font-size: 0.875rem; line-height: 1.5; color: var(--mr-text); }

    .comment-success-msg { padding: 1rem; background-color: rgba(5, 150, 105, 0.1); border: 1px solid rgba(5, 150, 105, 0.3); color: var(--mr-primary); border-radius: 0.5rem; font-size: 0.875rem; font-weight: 500; }

    /* Threading */
    .comment-reply { margin-left: 2rem; border-left: 3px solid var(--mr-border); padding-left: 1rem; border-radius: 0; border-top: none; border-right: none; border-bottom: none; background: transparent; }
    .mr-reply-btn { background: none; border: none; color: var(--mr-muted); font-size: 0.75rem; font-weight: 600; cursor: pointer; padding: 0; margin-top: 0.5rem; transition: color 0.2s; }
    .mr-reply-btn:hover { color: var(--mr-primary); }
    .mr-form-slot { margin-top: 1rem; }
    "###;

    (
        axum::http::StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
        css,
    )
        .into_response()
}

pub async fn serve_js() -> impl axum::response::IntoResponse {
    let js = r###"
(function() {
    const scriptTag = document.currentScript;
    const backendOrigin = new URL(scriptTag ? scriptTag.src : window.location.href).origin;

    const container = document.getElementById("markreply-comments");
    if (!container) return;

    const postSlug = container.getAttribute("data-slug") || window.location.pathname;

    if (!document.querySelector(`link[href="${backendOrigin}/widget.css"]`)) {
        const link = document.createElement("link");
        link.rel = "stylesheet";
        link.href = `${backendOrigin}/widget.css`;
        document.head.appendChild(link);
    }

    container.innerHTML = `
        <div class="mr-container">
            <h3 class="mr-title">Discussion</h3>

            <!-- 🌟 MOVED: The Master Slot (Form) is now ABOVE the comments list -->
            <div id="mr-master-slot" style="margin-bottom: 2rem;">
                <form id="mr-form" style="max-width: 36rem;">
                    <input type="hidden" id="mr-slug" value="${postSlug}">
                    <input type="hidden" id="mr-parent-id" value="">
                    <input type="text" id="mr-honeypot" name="honeypot" style="opacity: 0; position: absolute; top: 0; left: -9999px; z-index: -1;" tabindex="-1" autocomplete="off">

                    <div id="mr-replying-to" style="display: none; font-size: 0.75rem; color: var(--mr-primary); font-weight: 600; margin-bottom: 1rem;">
                        Replying to comment... <button type="button" id="mr-cancel-reply" style="background:none;border:none;color:var(--mr-muted);cursor:pointer;text-decoration:underline;">Cancel</button>
                    </div>

                    <div class="mr-grid">
                        <div>
                            <!-- 🌟 INCLUDED: Updated labels for theme compatibility -->
                            <label class="mr-label">Name *</label>
                            <input type="text" id="mr-name" required class="mr-input" placeholder="Jane Doe">
                        </div>
                        <div>
                            <label class="mr-label">Email (Optional)</label>
                            <input type="email" id="mr-email" class="mr-input" placeholder="jane@example.com">
                        </div>
                    </div>
                    <div style="margin-bottom: 1rem;">
                        <label class="mr-label">Comment *</label>
                        <textarea id="mr-content" rows="2" required class="mr-input" placeholder="Write a comment..." style="resize: vertical; min-height: 60px; max-height: 250px;"></textarea>
                    </div>
                    <!-- 🌟 FIX: Wrapped in a flex container to push the button right -->
                    <div style="display: flex; justify-content: space-between; align-items: center; margin-top: 0.5rem;">
                        <div id="mr-status"></div>
                        <button type="submit" class="mr-btn" id="mr-submit">Post Comment</button>
                    </div>
                </form>
            </div>

            <!-- 🌟 MOVED: The Comments List is now BELOW the form -->
            <div id="mr-list">
                <p style="color: var(--mr-muted); font-size: 0.875rem;">Loading comments...</p>
            </div>
        </div>
    `;

    const listEl = document.getElementById("mr-list");
    const formEl = document.getElementById("mr-form");
    const statusEl = document.getElementById("mr-status");
    const masterSlot = document.getElementById("mr-master-slot");
    const parentInput = document.getElementById("mr-parent-id");
    const replyIndicator = document.getElementById("mr-replying-to");

    window.mrReplyTo = function(commentId) {
        parentInput.value = commentId;
        replyIndicator.style.display = "block";
        const slot = document.getElementById(`mr-slot-${commentId}`);
        if (slot) slot.appendChild(formEl);
        document.getElementById("mr-content").focus();
    };

    document.getElementById("mr-cancel-reply").addEventListener("click", () => {
        parentInput.value = "";
        replyIndicator.style.display = "none";
        masterSlot.appendChild(formEl);
    });

    async function fetchComments() {
        try {
            const res = await fetch(`${backendOrigin}/api/comments?slug=${encodeURIComponent(postSlug)}`);
            listEl.innerHTML = await res.text();
        } catch (err) {
            listEl.innerHTML = '<p>Could not load comments.</p>';
        }
    }

    formEl.addEventListener("submit", async (e) => {
        e.preventDefault();
        document.getElementById("mr-submit").disabled = true;

        const formData = new URLSearchParams();
        formData.append("post_slug", postSlug);
        formData.append("author_name", document.getElementById("mr-name").value);
        formData.append("author_email", document.getElementById("mr-email").value);
        formData.append("content", document.getElementById("mr-content").value);
        formData.append("honeypot", document.getElementById("mr-honeypot").value);

        if (parentInput.value) {
            formData.append("parent_id", parentInput.value);
        }

        try {
            const res = await fetch(`${backendOrigin}/api/comments`, { method: "POST", body: formData });
            statusEl.innerHTML = await res.text();
            if (res.ok) {
                formEl.reset();
                parentInput.value = "";
                replyIndicator.style.display = "none";
                masterSlot.appendChild(formEl);

                fetchComments();
            }
        } finally {
            document.getElementById("mr-submit").disabled = false;
        }
    });

    fetchComments();
})();
    "###;
    (
        axum::http::StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        js,
    )
        .into_response()
}
