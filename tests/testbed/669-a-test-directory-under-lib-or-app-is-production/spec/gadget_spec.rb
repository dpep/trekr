describe Gadget do
  it { Gadget.new.from_lib_test; Gadget.new.from_app_test; Gadget::BOUNDARY }
  it { Gadget.new.from_lib_test; Gadget.new.from_app_test; Test::Result.new.score }
end
