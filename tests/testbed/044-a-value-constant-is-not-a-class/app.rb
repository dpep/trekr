module Router
  SEPARATORS = %w[/ .]

  def self.go
    SEPARATORS.join
  end
end

Short = Router
Short.go
