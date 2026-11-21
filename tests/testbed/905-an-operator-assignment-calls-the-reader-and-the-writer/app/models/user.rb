class User
  def self.bump
    w = Widget.new
    w.count ||= 0
    w.total += 1
    w.label &&= "x"
    w.size = w.size
  end
end
User.bump
