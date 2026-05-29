use clap::{Parser, Subcommand, Args};

#[derive(Parser)]
#[command(
    name = "mc-share",
    about = "Share your Minecraft world with a link — no port forwarding needed",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Host your Minecraft world and generate a share link
    Host(HostArgs),
    /// Join a shared Minecraft world via URL or room code
    Join(JoinArgs),
}

#[derive(Args)]
pub struct HostArgs {
    /// Minecraft server port (auto-detected from LAN world if omitted)
    #[arg(short, long, default_value = "0")]
    pub port: u16,

    /// Coordination server URL
    #[arg(long, env = "MCSHARE_COORD", default_value = "https://coord.minescale.example.com")]
    pub coord_url: String,

    /// STUN server for NAT traversal
    #[arg(long, default_value = "stun.l.google.com:19302")]
    pub stun_server: String,
}

#[derive(Args)]
pub struct JoinArgs {
    /// Share URL (https://mcs.sh/XXXXXX) or bare room code
    pub target: String,

    /// Local port to bind Minecraft proxy on (0 = auto)
    #[arg(short, long, default_value = "25565")]
    pub port: u16,

    /// Coordination server URL
    #[arg(long, env = "MCSHARE_COORD", default_value = "https://coord.minescale.example.com")]
    pub coord_url: String,

    /// STUN server for NAT traversal
    #[arg(long, default_value = "stun.l.google.com:19302")]
    pub stun_server: String,
}
