class Step
  def self.build(kind)
    kind.constantize.new
  end
end
