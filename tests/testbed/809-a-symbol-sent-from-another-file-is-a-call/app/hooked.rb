class Hooked
  def self.hook(*) = nil
  hook :on_save

  def on_save = 1
end
