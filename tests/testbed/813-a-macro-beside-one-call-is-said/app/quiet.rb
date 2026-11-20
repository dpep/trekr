class Quiet
  def self.skip_before_action(*) = nil
  skip_before_action :unhooked

  def go = hidden + unhooked
  def hidden = 1
  private :hidden
  def unhooked = 1
end
Quiet.new.go
