CREATE MIGRATION m1bgeql5ie4pvxxcs63vu5b5h74ft6qmfte2f4febgcwqzdbo4tusa
    ONTO m1mdwoyrh5c677pkvx57hvsxlsqiycrhijh6wyytflykey5essjiba
{
  ALTER TYPE default::Hello {
      CREATE PROPERTY foo: std::int64;
  };
};
