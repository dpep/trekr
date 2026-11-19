class Level
  def self.a2 = 1
  def a2 = 1
  send(:a2)
end
Level.a2
