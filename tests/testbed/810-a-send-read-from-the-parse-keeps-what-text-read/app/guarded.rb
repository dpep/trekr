class Guarded
  def go = Guarded.new.public_send(:shielded)

  protected

  def shielded = 1
end
Guarded.new.go
