class Widget
  def names(parts)
    parts.reject(&:empty?)
    %w[a b].map(&:upcase)
    [1, "a"].map(&:to_s)
  end
end
